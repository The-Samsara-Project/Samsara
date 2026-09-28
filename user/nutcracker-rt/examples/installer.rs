// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Samsara/Nutcracker interactive installer.
//
// This is the first interactive user-space program the kernel launches after
// the console server and keyboard driver. It:
//   * owns the display (FB_INFO + MAP_PHYS + CONSOLE_DETACH) and draws a
//     menu-driven setup wizard straight into the framebuffer,
//   * receives keystrokes by reading the console PTY slave (/dev/pts/0):
//     inputd -> consoled -> PTY master -> this process,
//   * writes its configuration to /etc (ramfs, or the ext2 root when one is
//     mounted over it),
//   * can launch and supervise the boot self-tests (forkx, pipetest, ...),
//   * on Finish detaches the kernel console (CONSOLE_DETACH), puts the console
//     PTY on the child's descriptor 0, spawns fbterm, and exits.
//
// All renderable text is ASCII: the 8x8 console font has no non-ASCII glyphs.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use nutcracker_rt::println;
use nutcracker_rt::syscall::{self, poll_events, PollFd};

// --- Embedded program indices (kernel/src/user.rs; append-only) ----------
// Boot servers    : consoled = 1, inputd = 2
// Self-test apps  : forkx = 3, pipetest = 4, credtst = 5, signaltst = 6,
//                   termiostst = 7, polltest = 8
// Shell = 9, term = 10, installer = 11.
const PROG_FORKX: u64 = 3;
const PROG_PIPETEST: u64 = 4;
const PROG_CREDTST: u64 = 5;
const PROG_SIGNALTST: u64 = 6;
const PROG_TERMIOS_TST: u64 = 7;
const PROG_POLLTEST: u64 = 8;
/// `busybox`, the ported userland. Spawned once, to make its own applet links.
const PROG_BUSYBOX: u64 = 15;
/// `chello`, the C program linked against the mlibc port. The only non-Rust
/// user image, so it is what shows the libc port runs and not merely links.
const PROG_CHELLO: u64 = 13;
/// `fbterm`, the ported Linux terminal emulator. The installer hands it the
/// display when setup finishes; see the `handoff` method.
const PROG_FBTERM: u64 = 14;

/// inputd's well-known IPC endpoint.
const EP_INPUTD: u64 = 3;
/// `args[0]` tag telling inputd to reload /etc/keymap.conf.
const RELOAD_LAYOUT: u64 = 1;

/// Configuration files under /etc (ramfs, or the mounted ext2 root).
const KEYMAP_CONF: &str = "/etc/keymap.conf";
const HOSTNAME_CONF: &str = "/etc/hostname";
const INIT_CONF: &str = "/etc/init.conf";

/// The setup wizard answers on this PTY slave for keyboard input.
const KEY_SLAVE: &str = "/dev/pts/0";

// --- Framebuffer rendering ------------------------------------------------
// The 8x8 console font, VGA palette and cell rasterizer are shared with the
// terminal emulator (ports/fbterm) via the runtime's `fb` module,
// so both display owners render text identically.
use nutcracker_rt::fb::{Display, BLACK, BLUE, CYAN, DGREY, LGREY, LGREEN, WHITE, YELLOW};

// --- Keyboard input -------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Tab,
    Backspace,
    Delete,
    Char(u8),
}

/// Poll `fd` briefly and pull whatever bytes are ready into `out`.
fn drain_fd(fd: usize, out: &mut Vec<u8>) {
    let mut pfd = [PollFd {
        fd: fd as i32,
        events: poll_events::POLLIN,
        revents: 0,
    }];
    if syscall::poll(&mut pfd, 40).unwrap_or(0) == 0 {
        return;
    }
    let mut chunk = [0u8; 64];
    if let Ok(n) = syscall::read(fd, &mut chunk) {
        if n > 0 {
            out.extend_from_slice(&chunk[..n]);
        }
    }
}

/// Pop the next complete key from `pending`. Partial escape sequences (a
/// bare `ESC` followed by `[`) are left alone until the rest arrives.
fn take_key(pending: &mut Vec<u8>) -> Option<Key> {
    while !pending.is_empty() {
        let b = pending[0];
        if b == 0x1B {
            if pending.len() >= 2 && pending[1] == b'[' {
                if pending.len() < 3 {
                    return None; // wait for the third byte
                }
                let fin = pending[2];
                if fin == b'3' && pending.len() >= 4 && pending[3] == b'~' {
                    pending.drain(..4);
                    return Some(Key::Delete);
                }
                pending.drain(..3);
                return Some(match fin {
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    _ => Key::Esc,
                });
            }
            pending.remove(0);
            return Some(Key::Esc);
        }
        pending.remove(0);
        return Some(match b {
            b'\n' | b'\r' => Key::Enter,
            0x08 | 0x7F => Key::Backspace,
            b'\t' => Key::Tab,
            _ => Key::Char(b),
        });
    }
    None
}

// --- Configuration file I/O ----------------------------------------------

/// Read `path` into `buf`; returns bytes read.
fn read_file(path: &str, buf: &mut [u8]) -> Result<usize, i64> {
    let fd = syscall::open(path, syscall::O_RDONLY, 0)?;
    let r = syscall::read(fd, buf);
    let _ = syscall::close(fd);
    r
}

/// Overwrite `path` with `data` (create if missing, truncate after open).
fn write_file(path: &str, data: &[u8]) -> Result<(), i64> {
    let fd = syscall::open(
        path,
        syscall::O_WRONLY | syscall::O_CREAT | syscall::O_TRUNC,
        0o644,
    )?;
    let r = syscall::write(fd, data);
    let _ = syscall::close(fd);
    r.map(|_| ())
}

/// First whitespace-separated token of the file, if it parses as UTF-8.
fn read_token(path: &str) -> Option<String> {
    let mut buf = [0u8; 128];
    let n = read_file(path, &mut buf).ok()?;
    let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
    text.split_whitespace().next().map(|t| String::from(t))
}

// --- Application state ----------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Screen {
    Welcome,
    Main,
    Overview,
    Layout,
    Hostname,
    Services,
    Storage,
    DiagMenu,
    About,
    Finish,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Nav {
    Stay,
    Next(Screen),
    Back,
    Quit,
}

/// One completed self-test result.
struct TestResult {
    name: String,
    pass: bool,
    status: i32,
}

struct App {
    fb: Display,
    pts: usize,
    pending: Vec<u8>,
    hostname: String,
    keymap: String,
    run_tests: bool,
    launch_term: bool,
    results: Vec<TestResult>,
    msg: Option<String>,
}

const LAYOUT_NAMES: [&str; 4] = ["us", "de", "colemak", "dvorak"];
const LAYOUT_DESC: [&str; 4] = [
    "US QWERTY",
    "German QWERTZ (Y/Z swapped)",
    "Colemak",
    "US Dvorak",
];

const MAIN_ITEMS: [(u8, &str); 8] = [
    (b'1', "System overview"),
    (b'2', "Keyboard layout"),
    (b'3', "Hostname"),
    (b'4', "Startup services"),
    (b'5', "Storage & disks"),
    (b'6', "Diagnostics"),
    (b'7', "About Samsara"),
    (b'8', "Finish installation"),
];

const TESTS: [(&str, u64); 7] = [
    ("forkx      (fork/exec/waitpid)", PROG_FORKX),
    ("pipetest   (pipes)", PROG_PIPETEST),
    ("signaltst  (signals)", PROG_SIGNALTST),
    ("termiostst (termios)", PROG_TERMIOS_TST),
    ("polltest   (poll)", PROG_POLLTEST),
    ("credtst    (credentials)", PROG_CREDTST),
    ("chello     (C/mlibc libc)", PROG_CHELLO),
];

impl App {
    /// Bring up the framebuffer view and the console PTY slave. The PTY
    /// appears as soon as consoled registers pair 0, so retry briefly.
    fn new() -> Option<App> {
        let fb = Display::new()?;
        let mut pts = None;
        for _ in 0..50 {
            if let Ok(fd) = syscall::open(KEY_SLAVE, syscall::O_RDWR, 0) {
                pts = Some(fd);
                break;
            }
            syscall::nanosleep(100).ok();
        }
        let pts = pts?;
        // The PTY line discipline (drivers/pty) follows termios like a real
        // terminal: by default it canonicalizes, echoes and edits the input.
        // The wizard renders its own prompt and decodes arrow-key escapes, so
        // put the slave into raw mode: no echo republished on the master
        // (that would pollute the framebuffer), no line editing, no ISIG.
        set_raw(pts);
        let hostname = read_token(HOSTNAME_CONF).unwrap_or_else(|| String::from("samsara"));
        let keymap = read_token(KEYMAP_CONF).unwrap_or_else(|| String::from("us"));
        Some(App {
            fb,
            pts,
            pending: Vec::new(),
            hostname,
            keymap,
            run_tests: true,
            launch_term: true,
            results: Vec::new(),
            msg: None,
        })
    }

    fn next_key(&mut self) -> Option<Key> {
        drain_fd(self.pts, &mut self.pending);
        take_key(&mut self.pending)
    }

    /// Block (with a periodic poll) until a key arrives.
    fn wait_key(&mut self) -> Key {
        loop {
            if let Some(k) = self.next_key() {
                return k;
            }
        }
    }

    /// Apply a keyboard layout: persist it, then nudge inputd to reload.
    fn apply_keymap(&mut self, name: &str) {
        let data = format!("{}\n", name);
        if let Err(e) = write_file(KEYMAP_CONF, data.as_bytes()) {
            self.msg = Some(format!("keymap: cannot write {}: {}", KEYMAP_CONF, e));
            return;
        }
        self.keymap = String::from(name);
        let mut frame = nutcracker_rt::ipc::MsgFrame::new();
        frame.args[0] = RELOAD_LAYOUT;
        frame.set_payload(data.as_bytes());
        let _ = nutcracker_rt::ipc::send(EP_INPUTD, &frame);
        self.msg = Some(format!("Keyboard layout switched to {} -- live now", name));
    }

    /// Persist the hostname to /etc/hostname.
    fn save_hostname(&mut self) {
        let data = format!("{}\n", self.hostname);
        if let Err(e) = write_file(HOSTNAME_CONF, data.as_bytes()) {
            self.msg = Some(format!("hostname: cannot write {}: {}", HOSTNAME_CONF, e));
        } else {
            self.msg = Some(format!("Hostname set to '{}'", self.hostname));
        }
    }

    /// Persist the boot options to /etc/init.conf (informational for now:
    /// both toggles are honoured live by this wizard).
    fn save_services(&mut self) {
        let data = format!(
            "run-tests={}\nlaunch-term={}\n",
            if self.run_tests { "yes" } else { "no" },
            if self.launch_term { "yes" } else { "no" }
        );
        if let Err(e) = write_file(INIT_CONF, data.as_bytes()) {
            self.msg = Some(format!("services: cannot write {}: {}", INIT_CONF, e));
        } else {
            self.msg = Some(String::from("Startup services saved to /etc/init.conf"));
        }
    }

    /// Probe every block device for an MBR/GPT/ext2-flavored sector 0 and
    /// return renderable lines.
    fn probe_storage(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        let mut buf = [0u8; 2048];
        let n = match read_file("/proc/devices", &mut buf) {
            Ok(n) => n,
            Err(e) => {
                out.push(format!("(could not list /proc/devices: {})", e));
                return out;
            }
        };
        let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
        // /proc/devices lines: "/dev/<name> <kind> -"
        let mut names = Vec::new();
        for line in text.lines() {
            let mut it = line.split_whitespace();
            if let (Some(path), Some(kind)) = (it.next(), it.next()) {
                if kind == "file" && path.starts_with("/dev/") {
                    names.push(String::from(path));
                }
            }
        }
        if names.is_empty() {
            out.push(String::from("no block devices found"));
            return out;
        }
        for name in &names {
            let mut st = syscall::Stat {
                mode: 0,
                uid: 0,
                gid: 0,
                kind: 0,
                size: 0,
            };
            let size = match syscall::stat(name, &mut st) {
                Ok(()) => st.size,
                Err(_) => 0,
            };
            let mut head = name.clone();
            if size >= 1 << 20 {
                head.push_str(&format!("  ({} MiB)", size >> 20));
            } else if size > 0 {
                head.push_str(&format!("  ({} KiB)", size >> 10));
            }
            // Sector 0 + the GPT header at LBA 1, when present.
            let mut sector = [0u8; 1024];
            let got = match syscall::open(name.as_str(), syscall::O_RDONLY, 0) {
                Ok(fd) => {
                    let r = syscall::read(fd, &mut sector);
                    let _ = syscall::close(fd);
                    r.unwrap_or(0)
                }
                Err(_) => 0,
            };
            if got < 512 {
                out.push(head);
                out.push(format!("  sector 0 unreadable ({} bytes)", got));
                continue;
            }
            // ext2 superblocks set the classic 0x55AA tail too, so an ext2
            // image without an MBR is reported as a filesystem below.
            let ext2 = sector[0x38] == 0x53 && sector[0x39] == 0xEF;
            let mbr = sector[0x1FE] == 0x55 && sector[0x1FF] == 0xAA && !ext2;
            let gpt = got >= 520 && sector[512..520] == *b"EFI PART";
            if gpt {
                out.push(head);
                out.push(format!("  GPT disk (EFI PART at LBA 1)"));
            } else if mbr {
                out.push(head);
                out.push(String::from("  MBR partition table"));
                for i in 0..4usize {
                    let off = 0x1BE + i * 16;
                    let ptype = sector[off + 4];
                    if ptype == 0x00 {
                        continue;
                    }
                    let lba = u32::from_le_bytes([
                        sector[off + 8],
                        sector[off + 9],
                        sector[off + 10],
                        sector[off + 11],
                    ]);
                    let count = u32::from_le_bytes([
                        sector[off + 12],
                        sector[off + 13],
                        sector[off + 14],
                        sector[off + 15],
                    ]);
                    let tyname = part_type(ptype);
                    let mi = (count as u64 * 512) >> 20;
                    out.push(format!(
                        "    [{}] lba {}  {} MiB  type 0x{:02X} {}",
                        i + 1,
                        lba,
                        mi,
                        ptype,
                        tyname
                    ));
                }
            } else if ext2 {
                out.push(head);
                out.push(String::from("  ext2/ext3/ext4 filesystem at LBA 0 (no MBR)"));
            } else {
                out.push(head);
                out.push(String::from("  unknown boot block (no MBR/GPT signature)"));
            }
        }
        out
    }

    /// Spawn one embedded test program, reap it, return (pass, exit status).
    fn run_one_test(&mut self, name: &str, prog: u64) -> (bool, i32) {
        match syscall::proc_spawn(prog, None) {
            Err(e) => {
                let _ = name;
                (false, e as i32)
            }
            Ok(pid) => {
                let mut status = 0i32;
                match syscall::waitpid(pid, &mut status) {
                    Ok(_) => (status == 0, status),
                    Err(e) => (false, e as i32),
                }
            }
        }
    }

    /// Run a subset (or all) of the self-tests, appending results.
    fn run_tests(&mut self, which: &[usize]) {
        self.results.clear();
        let mut idx = 0;
        for &i in which {
            let (name, prog) = TESTS[i];
            self.draw_test_progress(idx, which.len(), name, None);
            let (pass, status) = self.run_one_test(name, prog);
            self.results.push(TestResult {
                name: String::from(name),
                pass,
                status,
            });
            self.draw_test_progress(idx + 1, which.len(), name, Some((pass, status)));
            idx += 1;
        }
        // A short pause so the final "pass/fail" line is visible before the
        // summary screen renders.
        syscall::nanosleep(400).ok();
    }

    /// Live progress line for the diagnostics run.
    fn draw_test_progress(&self, done: usize, total: usize, name: &str, res: Option<(bool, i32)>) {
        let cols = self.fb.cols();
        let rows = self.fb.rows();
        let c0 = cols / 2 - 20;
        let c1 = cols / 2 + 20;
        let r0 = rows / 2 - 3;
        let r1 = rows / 2 + 3;
        self.fb.panel(c0, r0, c1, r1, "Selftests");
        let line = match res {
            None => format!("[{}] {} ...", done + 1, name),
            Some((true, _)) => format!("[{}] {} ... PASS", done, name),
            Some((false, s)) => format!("[{}] {} ... FAIL ({})", done, name, s),
        };
        self.fb.center(c0, c1, r0 + 2, &line, LGREY, BLACK);
        let pct = total.saturating_sub(1).max(1);
        let filled = done * 20 / pct;
        let mut bar = String::from("[");
        for i in 0..20 {
            bar.push(if i < filled { '#' } else { '.' });
        }
        bar.push(']');
        self.fb.center(c0, c1, r0 + 4, &bar, CYAN, BLACK);
    }

    /// Shared screen shell: header banner, footer hints, black content area.
    fn frame(&self, title: &str, hint: &str) {
        let cols = self.fb.cols();
        let rows = self.fb.rows();
        self.fb.clear(self.fb.packed(BLACK));
        // Header band (4 cells tall): wordmark + screen title sit vertically
        // centered, padded by a full empty cell row above and below so the
        // band does not feel cramped.
        self.fb.fill_rect(0, 0, self.fb.width, 32, self.fb.packed(BLUE));
        self.fb.center(0, cols - 1, 1, "SAMSARA - NUTCRACKER SETUP WIZARD", WHITE, BLUE);
        self.fb.center(0, cols - 1, 2, title, LGREY, BLUE);
        // Footer band (3 cells tall): the hint row is vertically centered so
        // it does not hug the bottom edge.
        self.fb.fill_rect(0, self.fb.height - 24, self.fb.width, 24, self.fb.packed(DGREY));
        self.fb.center(0, cols - 1, rows - 2, hint, WHITE, DGREY);
        // Status line just above the footer band.
        let status = format!(
            " hostname: {}    keymap: {}    tests: {}    term: {} ",
            self.hostname,
            self.keymap,
            if self.run_tests { "on" } else { "off" },
            if self.launch_term { "on" } else { "off" }
        );
        self.fb.blank(0, rows - 4, cols, BLACK);
        self.fb.put_text(2, rows - 4, &status, DGREY, BLACK);
        // One-shot message line under the header.
        if let Some(m) = &self.msg {
            self.fb.blank(2, 4, cols - 4, BLACK);
            self.fb.put_text(2, 4, m, YELLOW, BLACK);
        }
    }

    /// Release the display to fbterm, start it, and exit.
    ///
    /// The order here is the whole point, and each step exists because the one
    /// after it would otherwise be undone or would not work:
    ///
    /// 1. **Put the pty slave on descriptor 0.** fbterm reads its keystrokes
    ///    from stdin, so stdin has to be the terminal the keyboard driver is
    ///    already feeding -- which is this process's `pts` descriptor, not
    ///    `/dev/console`. `dup2` rather than a plain re-open because the child
    ///    must *share* the same open file description: it inherits descriptors
    ///    from us, and sharing is what keeps the line discipline in effect on
    ///    both ends. Descriptor 0 is replaced; the wizard's own descriptor is
    ///    left alone and is about to be closed by exit anyway.
    ///
    /// 2. **Detach the kernel console.** Until this point the kernel paints its
    ///    own text over the framebuffer, and `/dev/console` writes to the
    ///    framebuffer as well as the serial line. fbterm opens `/dev/fb0` and
    ///    draws, so anything still painting would scribble over it -- and
    ///    because `consoled` echoes every keystroke to the console, the symptom
    ///    would be text reappearing over the terminal a second after it
    ///    started, which reads as fbterm misrendering rather than as two
    ///    programs sharing one framebuffer. Detaching stops the painting and
    ///    keeps the serial log, so diagnostics are unaffected.
    ///
    /// 3. **Spawn fbterm.** Descriptors 1 and 2 are deliberately left as
    ///    `/dev/console`, which after the detach means the serial line only.
    ///    That is where fbterm's diagnostics should go: its drawing goes to the
    ///    framebuffer directly, and anything it prints is a message about the
    ///    program rather than part of its output.
    ///
    /// The detach happens *before* the spawn so that nothing paints over
    /// fbterm's first frame, and the completion message is printed after the
    /// detach too -- which means it reaches the serial log and not the screen,
    /// which is right: the screen belongs to fbterm from here on.
    /// Populate `/bin` with the userland's applet links.
    ///
    /// The installer does this itself, as root, rather than handing the job to
    /// `busybox --install -s /bin`. The reason is ownership: `/bin` is root-owned
    /// and mode 0755, which is correct -- a userland directory that any process
    /// can add names to is a directory nobody can reason about -- and busybox is
    /// deliberately *not* privileged. Run as the user it is meant to be, every one
    /// of its 106 `symlink` calls fails with EACCES, and the failure is silent in
    /// the sense that matters: the applet still works when named explicitly, so
    /// the system looks almost right and `/bin/ls` alone is missing.
    ///
    /// The list comes from `busybox --list`, which is the binary's own answer to
    /// "which applets are in this build". Hardcoding it here would be a second
    /// copy that goes stale when the port's config changes, and a stale copy means
    /// `/bin/foo` missing while `busybox foo` works -- which reads as a broken
    /// kernel rather than a forgotten edit.
    ///
    /// So: run busybox with stdout on a pipe, read the names, link each one.
    fn install_userland(&mut self) {
        let (rd, wr) = match syscall::pipe() {
            Ok(p) => p,
            Err(e) => {
                println!("[installer] could not make a pipe: {}", e);
                return;
            }
        };
        // Put the console back afterwards, whatever happens. Two descriptors are
        // involved and the order matters:
        //
        //   dup(1)   a spare, because `dup2` returns the *new* descriptor, not the
        //             old one. Saving its return value saves 1, and closing that
        //             would close the pipe we just installed rather than the
        //             console -- leaving the child with no stdout at all, which
        //             shows up as an empty read and reads as "busybox printed
        //             nothing" rather than as a descriptor mistake.
        //   dup2(wr,1) point stdout at the pipe
        //   close(wr)  drop the spare, so the pipe has exactly one write end and
        //             read-to-EOF below can actually reach EOF
        let saved_stdout = syscall::dup(1);
        if syscall::dup2(wr as u64, 1).is_err() {
            println!("[installer] could not redirect stdout");
            syscall::close(rd);
            syscall::close(wr);
            return;
        }
        syscall::close(wr);

        // argv[0] is the program's own name. `/bin/busybox` rather than
        // "busybox" because the loader and busybox both take the applet hint from
        // it, and a relative name would be resolved against the installer's
        // working directory rather than against /bin.
        let argv: [&str; 2] = ["/bin/busybox", "--list"];
        let pid = match syscall::proc_spawn(PROG_BUSYBOX, Some(&argv)) {
            Ok(p) => p,
            Err(e) => {
                println!("[installer] could not run busybox --list: {}", e);
                return;
            }
        };

        // Drain to end-of-file, then close. The child inherited the installer's
        // descriptor table, so at the moment of the spawn it held fd 1 -- the pipe
        // -- and that copy is what closes when it exits, which is the EOF this is
        // waiting for. Our own spare write end was already closed above, so there
        // is exactly one and it belongs to the child.
        let names = read_pipe_lines(rd);
        syscall::close(rd);

        let mut status = 0i32;
        let _ = syscall::waitpid(pid, &mut status);

        // The console back on fd 1, so every later `println!` is visible again.
        if let Ok(saved) = saved_stdout {
            let _ = syscall::dup2(saved as u64, 1);
            syscall::close(saved);
        }

        if names.is_empty() {
            println!("[installer] busybox --list produced nothing; /bin left empty");
            return;
        }

        let mut made = 0usize;
        let mut failed = 0usize;
        for name in &names {
            let path = format!("/bin/{}", name);
            match syscall::symlink("/bin/busybox", &path) {
                // EEXIST is success in effect: the name is already reachable by
                // this path, which is the only thing the link was for.
                Ok(()) => made += 1,
                Err(-17) => made += 1,
                Err(_) => failed += 1,
            }
        }
        // `sh` is not a separate applet -- this busybox is configured with
        // CONFIG_SH_IS_ASH, so the shell's *name* is `ash` -- but everything in a
        // system expects /bin/sh, and the shell is resolved from argv[0] like any
        // other applet. Linked by hand, because it is a name the binary does not
        // report under `--list` and so no amount of asking would produce it.
        //
        // EEXIST counts as made: on a system where `sh` *is* in the applet table
        // the link above has already created it, and the name is reachable either
        // way, which is the only thing the link was for.
        match syscall::symlink("/bin/busybox", "/bin/sh") {
            Ok(()) | Err(-17) => made += 1,
            Err(_) => failed += 1,
        }
        println!(
            "[installer] /bin populated: {} links, {} failed",
            made, failed
        );

        // Prove it, rather than assume it.
        //
        // A link that exists is not a link that *resolves*, and the difference is
        // exactly what applet dispatch depends on. So a program is run through its
        // own path, by path, with no help from the program table: `execve` on
        // `/bin/echo` is the only route to the `echo` applet, and if that fails
        // then `/bin` is a directory of decoration no matter how many links it
        // contains.
        //
        // Each applet is checked in a child so that the exec happens in a process
        // this one can survive: a successful execve never returns, so a failed one
        // has to be observable through an exit status rather than through a
        // return value.
        for probe in ["/bin/true", "/bin/echo", "/bin/ls"] {
            // argv[0] is the path, which is what the applet dispatch reads.
            // `true` and `echo` need no argument; `ls` lists /bin, which is a
            // real listing and exercises the link, the directory read behind it,
            // and the program's own output path in one go.
            let argv: [&str; 2] = [probe, "--"];

            // Each probe execs in a child, because a successful execve does not
            // return: a failure can only be observed through an exit status.
            let child = syscall::fork();
            if child <= 0 {
                if child < 0 {
                    println!("[installer] could not fork for {}: {}", probe, child);
                } else {
                    match syscall::execve(probe, &argv) {
                        // Unreachable in practice: the exec either replaces this
                        // process or reports why it could not.
                        Ok(()) => syscall::proc_exit_code(0),
                        Err(e) => {
                            println!("[installer] execve {} failed: {}", probe, e);
                            syscall::proc_exit_code(1);
                        }
                    }
                }
                continue;
            }
            let mut st = 0i32;
            match syscall::waitpid(child as u64, &mut st) {
                Ok(_) if st == 0 => println!("[installer] {} runs as an applet", probe),
                Ok(_) => println!("[installer] {} exited {}", probe, st),
                Err(e) => println!("[installer] {} unreaped: {}", probe, e),
            }
        }
    }

    fn handoff(&mut self) -> ! {
        // Before the framebuffer changes hands. Once fbterm is running it owns the
        // display and anything written to the console afterwards is invisible, so
        // a step that reports itself has to report itself first.
        self.install_userland();
        // 1. The child reads keys from the pty, so give it the pty on stdin.
        let pts = self.pts;
        if syscall::dup2(pts as u64, 0).is_err() {
            println!("[installer] could not put the terminal on stdin");
        }

        // 2. Stop the kernel console painting over the display.
        if syscall::console_detach().is_err() {
            println!("[installer] could not release the framebuffer");
        }

        // 3. Start the terminal.
        if self.launch_term {
            println!("[installer] handing the display to fbterm");
            let _ = syscall::proc_spawn(PROG_FBTERM, None);
        }
        println!(
            "[installer] setup complete (hostname={}, keymap={})",
            self.hostname, self.keymap
        );
        syscall::proc_exit_code(0);
    }
}

/// Read a pipe to end-of-file and return its non-empty lines.
///
/// End-of-file, not "whatever is in the buffer now": the child is still running
/// when this is called, and a read that returned after one short chunk would
/// truncate the list at whatever fitted. A child that exits without closing would
/// block here forever, which is the correct behaviour for a pipe read and not
/// something to work around with a timeout -- the alternative is a half-installed
/// `/bin` that reports success.
///
/// One byte is dropped from each line's length and then excluded from the
/// accumulator, so the buffer is a sliding window rather than being copied into a
/// second allocation per chunk.
fn read_pipe_lines(fd: usize) -> Vec<String> {
    const CHUNK: usize = 1024;
    let mut buf = [0u8; CHUNK];
    let mut acc: Vec<u8> = Vec::new();
    let mut lines: Vec<String> = Vec::new();

    loop {
        let n = match syscall::read(fd, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let mut consumed = 0usize;
        for i in 0..n {
            if buf[i] == b'\n' {
                if i > consumed {
                    acc.extend_from_slice(&buf[consumed..i]);
                }
                if !acc.is_empty() {
                    if let Ok(s) = core::str::from_utf8(&acc) {
                        let t = s.trim();
                        if !t.is_empty() {
                            lines.push(String::from(t));
                        }
                    }
                }
                acc.clear();
                consumed = i + 1;
            }
        }
        if consumed < n {
            acc.extend_from_slice(&buf[consumed..n]);
        }
    }
    // A last line with no trailing newline is still a line.
    if !acc.is_empty() {
        if let Ok(s) = core::str::from_utf8(&acc) {
            let t = s.trim();
            if !t.is_empty() {
                lines.push(String::from(t));
            }
        }
    }
    lines
}

/// Convert owned menu rows into the borrowed form the renderer wants, so
/// screen code can snapshot state without holding borrows across input waits.
fn menu_refs(owned: &[(u8, String, String)]) -> Vec<(u8, &str, &str)> {
    owned
        .iter()
        .map(|(h, l, v)| (*h, l.as_str(), v.as_str()))
        .collect()
}

/// Put a termios device into raw mode: no canonicalization, echo, ISIG
/// interception or CR/LF or OPOST translation, so bytes pass through exactly
/// as written.
fn set_raw(fd: usize) {
    let t = core::mem::MaybeUninit::<syscall::Termios>::uninit();
    let mut t = unsafe { t.assume_init() };
    if syscall::ioctl(fd, syscall::TCGETS, &mut t).is_err() {
        return;
    }
    t.c_lflag &= !(syscall::ISIG | syscall::ICANON | syscall::ECHO | syscall::ECHOE | syscall::ECHOK);
    t.c_iflag &= !(syscall::ICRNL | syscall::INLCR | syscall::IGNCR);
    t.c_oflag &= !syscall::OPOST;
    let _ = syscall::ioctl(fd, syscall::TCSETS, &mut t);
}

/// Give an MBR partition type a human name.
fn part_type(t: u8) -> &'static str {
    match t {
        0x01 => "fat12",
        0x04 | 0x06 => "fat16",
        0x05 | 0x0F => "extended",
        0x07 => "ntfs/exfat",
        0x0B | 0x0C => "fat32",
        0x82 => "swap",
        0x83 => "linux",
        0x8E => "linux-lvm",
        0xFD => "linux-raid",
        0xEE => "gpt-mbr",
        0xEF => "efi-sys",
        0x00 => "empty",
        _ => "unknown",
    }
}

// --- Screens --------------------------------------------------------------

/// Welcome: introductory screen; any key proceeds.
fn screen_welcome(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    app.frame("Welcome", "press any key to continue");
    let mut lines = Vec::new();
    lines.push(String::from("Welcome to the Samsara setup wizard."));
    lines.push(String::new());
    lines.push(String::from(
        "This system is built on the Nutcracker runtime: a small kernel,",
    ));
    lines.push(String::from(
        "userspace servers and native drivers. Configuration you make here",
    ));
    lines.push(String::from(
        "is applied immediately and saved under /etc.",
    ));
    lines.push(String::new());
    lines.push(String::from(
        "Navigate with the arrow keys and Enter.  ESC jumps back a screen;",
    ));
    lines.push(String::from("q quits to the Finish screen at any time."));
    let c0 = cols / 2 - 36;
    let c1 = cols / 2 + 36;
    let r0 = rows / 2 - 6;
    let r1 = rows / 2 + 8;
    app.fb.text_block(c0, r0, c1, r1, "Introduction", &lines);
    app.wait_key();
    Nav::Next(Screen::Main)
}

/// Main menu: eight choices.
fn screen_main(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    let mut sel = 0usize;
    loop {
        app.frame("Main menu", "up/down or number key to select, Enter to open, Esc/q to finish");
        let mut owned: Vec<(u8, String, String)> = Vec::new();
        for (i, (hotkey, label)) in MAIN_ITEMS.iter().enumerate() {
            let value = match i {
                1 => String::from(app.keymap.as_str()),
                2 => String::from(app.hostname.as_str()),
                3 => {
                    if app.run_tests && app.launch_term {
                        String::from("tests+term")
                    } else if app.run_tests {
                        String::from("tests only")
                    } else if app.launch_term {
                        String::from("term only")
                    } else {
                        String::from("off")
                    }
                }
                5 if !app.results.is_empty() => String::from("rerun ok"),
                _ => String::new(),
            };
            owned.push((*hotkey, String::from(*label), value));
        }
        let items = menu_refs(&owned);
        let c0 = cols / 2 - 24;
        let c1 = cols / 2 + 26;
        let r0 = rows / 2 - 9;
        let r1 = rows / 2 + 11;
        app.fb.menu(c0, r0, c1, r1, "What would you like to do?", &items, sel, "Esc/q = skip to finish");
        match app.wait_key() {
            Key::Up => sel = (sel + 7) % 8,
            Key::Down => sel = (sel + 1) % 8,
            Key::Char(c) if (b'1'..=b'8').contains(&c) => {
                sel = (c - b'1') as usize;
                return Nav::Next(match sel {
                    0 => Screen::Overview,
                    1 => Screen::Layout,
                    2 => Screen::Hostname,
                    3 => Screen::Services,
                    4 => Screen::Storage,
                    5 => Screen::DiagMenu,
                    6 => Screen::About,
                    _ => Screen::Finish,
                });
            }
            Key::Enter => {
                return Nav::Next(match sel {
                    0 => Screen::Overview,
                    1 => Screen::Layout,
                    2 => Screen::Hostname,
                    3 => Screen::Services,
                    4 => Screen::Storage,
                    5 => Screen::DiagMenu,
                    6 => Screen::About,
                    _ => Screen::Finish,
                });
            }
            Key::Esc | Key::Char(b'q') => return Nav::Next(Screen::Finish),
            _ => {}
        }
    }
}

/// System overview: kernel version, uptime, memory, processes.
fn screen_overview(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    app.frame(
        "System overview",
        "ESC = back to main menu,  q = finish",
    );
    let mut lines = Vec::new();
    let mut buf = [0u8; 512];
    match read_file("/proc/version", &mut buf) {
        Ok(n) if n > 0 => {
            lines.push(format!(
                "kernel : {}",
                core::str::from_utf8(&buf[..n.min(64)]).unwrap_or("?")
            ));
        }
        _ => lines.push(String::from("kernel : (unknown)")),
    }
    let secs = syscall::uptime_ms() / 1000;
    lines.push(format!("uptime : {}s", secs));
    match read_file("/proc/meminfo", &mut buf) {
        Ok(n) if n > 0 => {
            let text = core::str::from_utf8(&buf[..n]).unwrap_or("");
            for line in text.lines() {
                let mut it = line.split_whitespace();
                let key = it.next().unwrap_or("?");
                let val = it.next().unwrap_or("0");
                match key {
                    "total_kib" => lines.push(format!("memory : {} MiB total", val.parse::<u64>().unwrap_or(0) >> 10)),
                    "used_kib" => lines.push(format!("         {} MiB used", val.parse::<u64>().unwrap_or(0) >> 10)),
                    "free_kib" => lines.push(format!("         {} MiB free", val.parse::<u64>().unwrap_or(0) >> 10)),
                    "heap_mib" => lines.push(format!("heap    : {} MiB", val)),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    lines.push(String::new());
    lines.push(String::from("Processes:"));
    let mut tbuf = [0u8; 1024];
    match read_file("/proc/tasks", &mut tbuf) {
        Ok(n) if n > 0 => {
            let text = core::str::from_utf8(&tbuf[..n]).unwrap_or("");
            for (i, line) in text.lines().enumerate() {
                if i == 0 {
                    continue;
                }
                lines.push(String::from("  ") + line);
                if lines.len() >= 20 {
                    break;
                }
            }
        }
        _ => {}
    }
    let c0 = 6;
    let c1 = cols - 7;
    let r0 = 7;
    let r1 = rows - 8;
    app.fb.text_block(c0, r0, c1, r1, "Live view of /proc", &lines);
    app.wait_key();
    Nav::Back
}

/// Keyboard layout picker.
fn screen_layout(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    let mut sel = LAYOUT_NAMES
        .iter()
        .position(|n| *n == app.keymap)
        .unwrap_or(0);
    loop {
        app.frame("Keyboard layout", "up/down + Enter to activate, ESC = back, q = finish");
        let mut owned: Vec<(u8, String, String)> = Vec::new();
        for (i, name) in LAYOUT_NAMES.iter().enumerate() {
            let active = *name == app.keymap;
            let value = format!(
                "{}{}",
                LAYOUT_DESC[i],
                if active { "   <active>" } else { "" }
            );
            owned.push((0, String::from(*name), value));
        }
        let items = menu_refs(&owned);
        let c0 = cols / 2 - 26;
        let c1 = cols / 2 + 26;
        let r0 = rows / 2 - 6;
        let r1 = rows / 2 + 6;
        app.fb.menu(c0, r0, c1, r1, "Layout", &items, sel, "Enter applies and notifies inputd live");
        match app.wait_key() {
            Key::Up => sel = (sel + 3) % 4,
            Key::Down => sel = (sel + 1) % 4,
            Key::Enter => {
                app.apply_keymap(LAYOUT_NAMES[sel]);
                return Nav::Stay;
            }
            Key::Esc => return Nav::Back,
            Key::Char(b'q') => return Nav::Quit,
            _ => {}
        }
    }
}

/// Hostname editor.
fn screen_hostname(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    let mut name: Vec<u8> = app.hostname.as_bytes().to_vec();
    let mut done = false;
    while !done {
        app.frame("Hostname", "type a hostname, Enter saves, ESC = back, q = finish");
        let c0 = cols / 2 - 20;
        let c1 = cols / 2 + 20;
        let r0 = rows / 2 - 3;
        let r1 = rows / 2 + 3;
        app.fb.panel(c0, r0, c1, r1, "Hostname");
        let mut cur = String::from("> ");
        cur.push_str(core::str::from_utf8(&name).unwrap_or(""));
        cur.push('_');
        app.fb.center(c0, c1, r0 + 2, &cur, LGREEN, BLACK);
        match app.wait_key() {
            Key::Char(b) => {
                if name.len() < 48 && (b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_') {
                    name.push(b);
                }
            }
            Key::Backspace => {
                name.pop();
            }
            Key::Enter => {
                if !name.is_empty() {
                    app.hostname = String::from_utf8_lossy(&name).into_owned();
                    app.save_hostname();
                    done = true;
                }
            }
            Key::Esc => done = true,
            _ => {}
        }
    }
    Nav::Back
}

/// Startup services: which post-install actions run.
fn screen_services(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    let mut sel = 0usize;
    let mut dirty = false;
    loop {
        app.frame("Startup services", "space/Enter toggles, ESC saves + back, q = finish");
        let owned = vec![
            (
                0,
                String::from("run boot self-tests"),
                String::from(if app.run_tests { "[x]" } else { "[ ]" }),
            ),
            (
                0,
                String::from("hand the display to fbterm"),
                String::from(if app.launch_term { "[x]" } else { "[ ]" }),
            ),
        ];
        let items = menu_refs(&owned);
        let c0 = cols / 2 - 24;
        let c1 = cols / 2 + 24;
        let r0 = rows / 2 - 3;
        let r1 = rows / 2 + 5;
        app.fb.menu(c0, r0, c1, r1, "What happens on Finish?", &items, sel, "settings land in /etc/init.conf");
        match app.wait_key() {
            Key::Up => sel = (sel + 1) % 2,
            Key::Down => sel = (sel + 1) % 2,
            Key::Enter | Key::Char(b' ') => {
                if sel == 0 {
                    app.run_tests = !app.run_tests;
                } else {
                    app.launch_term = !app.launch_term;
                }
                dirty = true;
            }
            Key::Esc => {
                if dirty {
                    app.save_services();
                }
                return Nav::Back;
            }
            Key::Char(b'q') => return Nav::Quit,
            _ => {}
        }
    }
}

/// Storage probe.
fn screen_storage(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    app.frame(
        "Storage and disks",
        "ESC = back to main menu,  q = finish",
    );
    let lines = app.probe_storage();
    let c0 = 6;
    let c1 = cols - 7;
    let r0 = 7;
    let r1 = rows - 8;
    app.fb.text_block(c0, r0, c1, r1, "Block device survey", &lines);
    app.wait_key();
    Nav::Back
}

/// Diagnostics menu.
fn screen_diag_menu(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    let mut sel = 0usize;
    loop {
        app.frame("Diagnostics", "Enter runs the selected test; ESC = back, q = finish");
        let mut owned: Vec<(u8, String, String)> = Vec::new();
        owned.push((
            0,
            String::from("Run all six self-tests"),
            String::from(if app.results.is_empty() { "" } else { "again" }),
        ));
        for (name, _prog) in TESTS.iter() {
            let mark = app
                .results
                .iter()
                .find(|r| r.name.as_str() == *name)
                .map(|r| if r.pass { "[PASS]" } else { "[FAIL]" })
                .unwrap_or("");
            owned.push((0, String::from(*name), String::from(mark)));
        }
        let items = menu_refs(&owned);
        let c0 = cols / 2 - 26;
        let c1 = cols / 2 + 26;
        let r0 = rows / 2 - 8;
        let r1 = rows / 2 + 10;
        app.fb.menu(c0, r0, c1, r1, "Selftests", &items, sel, "each test runs in its own process; exit 0 = pass");
        match app.wait_key() {
            Key::Up => sel = (sel + items.len() - 1) % items.len(),
            Key::Down => sel = (sel + 1) % items.len(),
            Key::Enter => {
                if sel == 0 {
                    let all: Vec<usize> = (0..TESTS.len()).collect();
                    app.run_tests(&all);
                } else {
                    app.run_tests(&[sel - 1]);
                }
            }
            Key::Esc => return Nav::Back,
            Key::Char(b'q') => return Nav::Quit,
            _ => {}
        }
    }
}

/// About: system blurb.
fn screen_about(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    app.frame("About Samsara", "ESC = back to main menu,  q = finish");
    let lines = [
        String::from("Samsara is a small operating system built around the"),
        String::from("Nutcracker runtime: a microkernel-style core with device"),
        String::from("drivers split from the kernel into user-space servers."),
        String::new(),
        String::from("  kernel     : x86_64, preemptive, per-process address"),
        String::from("               spaces, POSIX-flavoured syscalls"),
        String::from("  servers    : consoled (PTY), inputd (keyboard)"),
        String::from("  drivers    : VGA/EDID framebuffer, AHCI SATA, NVMe"),
        String::from("  filesystem : ramfs + ext2 under test, devfs, procfs"),
        String::new(),
        String::from("This wizard is the boot-time front end: it configures the"),
        String::from("system, runs the self-test suite on demand, then hands"),
        String::from("the display to fbterm for normal use."),
    ];
    let c0 = cols / 2 - 40;
    let c1 = cols / 2 + 40;
    let r0 = rows / 2 - 8;
    let r1 = rows / 2 + 9;
    app.fb.text_block(c0, r0, c1, r1, "About", &lines);
    app.wait_key();
    Nav::Back
}

/// Finish: summary, optional final test pass, then hand the display back.
fn screen_finish(app: &mut App) -> Nav {
    let cols = app.fb.cols();
    let rows = app.fb.rows();
    app.frame("Finish", "Enter hands the display to fbterm, ESC = back");
    let mut lines = Vec::new();
    lines.push(format!("Hostname            : {}", app.hostname));
    lines.push(format!("Keyboard layout     : {}", app.keymap));
    lines.push(format!("Run self-tests      : {}", if app.run_tests { "yes" } else { "no" }));
    lines.push(format!("Launch terminal     : {}", if app.launch_term { "yes" } else { "no" }));
    lines.push(String::new());
    if app.run_tests && app.results.is_empty() {
        lines.push(String::from(
            "A final self-test pass will run before the handoff (press Enter).",
        ));
    }
    lines.push(String::from(
        "The kernel console is detached (CONSOLE_DETACH), then fbterm",
    ));
    lines.push(String::from(
        "takes the display and starts a shell on its own PTY.",
    ));
    let c0 = cols / 2 - 35;
    let c1 = cols / 2 + 35;
    let r0 = rows / 2 - 6;
    let r1 = rows / 2 + 7;
    app.fb.text_block(c0, r0, c1, r1, "Summary", &lines);
    loop {
        match app.wait_key() {
            Key::Enter => {
                if app.run_tests && app.results.is_empty() {
                    let all: Vec<usize> = (0..TESTS.len()).collect();
                    app.frame("Finish", "running final self-test pass...");
                    app.run_tests(&all);
                    app.frame("Finish", "Enter hands the display to fbterm, ESC = back");
                    let mut lines = Vec::new();
                    let mut ok = 0;
                    for r in &app.results {
                        if r.pass {
                            ok += 1;
                        }
                    }
                    lines.push(format!("{} of {} tests passed.", ok, app.results.len()));
                    for r in &app.results {
                        if !r.pass {
                            lines.push(format!("  FAIL: {} (status {})", r.name, r.status));
                        }
                    }
                    lines.push(String::from("Press Enter to boot into the terminal."));
                    let c0 = cols / 2 - 35;
                    let c1 = cols / 2 + 35;
                    let r0 = rows / 2 - 5;
                    let r1 = rows / 2 + 6;
                    app.fb.text_block(c0, r0, c1, r1, "Test results", &lines);
                    app.wait_key();
                }
                app.handoff();
            }
            Key::Esc => return Nav::Back,
            Key::Char(b'q') => {
                app.handoff();
            }
            _ => {}
        }
    }
}

// --- Entry point ----------------------------------------------------------

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[installer] setup wizard online (epid {})", syscall::get_epid());
    let mut app = match App::new() {
        Some(a) => a,
        None => {
            // No framebuffer or no keyboard: there is no display to run a
            // terminal on, so there is nothing to hand off to. This used to
            // start the terminal emulator anyway, which could only have failed
            // for the same reason -- it needs the framebuffer too.
            //
            // So say what is missing and stop, rather than spawning a program
            // that is certain to fail and leaving the reason to be guessed at
            // from its absence.
            println!("[installer] no framebuffer or keyboard; cannot run a terminal");
            println!("[installer] the serial log above is all the output available");
            syscall::proc_exit_code(1);
        }
    };
    println!(
        "[installer] framebuffer {}x{} @{}bpp, keys via {}",
        app.fb.width, app.fb.height, app.fb.pxsize * 8, KEY_SLAVE
    );
    // Own the display before painting anything.
    let _ = syscall::console_detach();
    app.fb.clear(app.fb.packed(BLACK));
    // Drop any keystrokes typed before the banner came up.
    while app.next_key().is_some() {}

    // Screen machine.
    let mut cur = Screen::Welcome;
    loop {
        let nav = match cur {
            Screen::Welcome => screen_welcome(&mut app),
            Screen::Main => screen_main(&mut app),
            Screen::Overview => screen_overview(&mut app),
            Screen::Layout => screen_layout(&mut app),
            Screen::Hostname => screen_hostname(&mut app),
            Screen::Services => screen_services(&mut app),
            Screen::Storage => screen_storage(&mut app),
            Screen::DiagMenu => screen_diag_menu(&mut app),
            Screen::About => screen_about(&mut app),
            Screen::Finish => screen_finish(&mut app),
        };
        cur = match nav {
            Nav::Stay => cur,
            Nav::Next(s) => s,
            Nav::Back => match cur {
                Screen::Main => Screen::Finish,
                _ => Screen::Main,
            },
            Nav::Quit => Screen::Finish,
        };
    }
}
