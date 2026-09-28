// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Native terminal emulator. After the setup wizard hands the display over,
// this process owns the framebuffer (FB_INFO + MAP_PHYS + CONSOLE_DETACH)
// and renders the PTY output itself: an ANSI parser drives an in-memory
// cell screen, and only dirty cells are repainted. A shell spawned on the
// slave side (`/dev/pts0`) therefore gets a real terminal: cursor motion,
// colours, erase and scroll all work.
//
// Input never touches this process: `inputd` decodes PS/2 scancodes and
// `consoled` writes the resulting bytes into the same PTY master; the
// kernel's line discipline (drivers/pty) echoes, edits and canonicalizes
// them, and the echo flows back through the master to this renderer.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use nutcracker_rt::fb::{self, Display};
use nutcracker_rt::println;
use nutcracker_rt::syscall::{self, poll_events, PollFd};

/// Index of the shell (kernel/src/user.rs).
const PROG_SHELL: u64 = 9;
/// Slave device of PTY pair 0, where the shell runs.
const SLAVE: &str = "/dev/pts0";

/// The master end of pair 0 -- the other end of the same pty as [`SLAVE`].
/// Not `/dev/ptmx`, which is an allocating multiplexer; see the open below.
const MASTER: &str = "/dev/ptmx0";

/// Default pen: light-grey on black, like the VGA console.
const DEF_FG: u8 = fb::LGREY;
const DEF_BG: u8 = fb::BLACK;

// --- ANSI control writer (what the emulator accepts) -----------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Ground,
    Esc,
    Csi,
    Osc,
}

struct Parser {
    mode: Mode,
    params: [u16; 8],
    nparams: usize,
    param: u16,
    private: bool,
}

impl Parser {
    fn new() -> Self {
        Self {
            mode: Mode::Ground,
            params: [0; 8],
            nparams: 0,
            param: 0,
            private: false,
        }
    }

    fn reset_csi(&mut self) {
        self.params = [0; 8];
        self.nparams = 0;
        self.param = 0;
        self.private = false;
    }

    fn push_param(&mut self) {
        if self.nparams < self.params.len() {
            self.params[self.nparams] = self.param;
            self.nparams += 1;
        }
        self.param = 0;
    }

    /// First parameter with a default of 1 (cursor motions); zero if none.
    fn p0(&self) -> usize {
        if self.nparams == 0 {
            1
        } else {
            self.params[0] as usize
        }
    }

    fn feed(&mut self, bytes: &[u8], scr: &mut Screen, reply: &mut dyn FnMut(&[u8])) {
        for &b in bytes {
            self.step(b, scr, reply);
        }
    }

    fn step(&mut self, b: u8, scr: &mut Screen, reply: &mut dyn FnMut(&[u8])) {
        match self.mode {
            Mode::Ground => match b {
                0x1b => self.mode = Mode::Esc,
                b'\r' => scr.cr(),
                b'\n' => scr.lf(),
                0x08 => scr.bs(),
                b'\t' => scr.tab(),
                0x07 => {} // BEL: no speaker, ignore
                0x0b | 0x0c => scr.lf(),
                _ if b < 0x20 => {}
                _ => scr.put(b),
            },
            Mode::Esc => match b {
                b'[' => {
                    self.reset_csi();
                    self.mode = Mode::Csi;
                }
                b'7' => scr.save(),
                b'8' => scr.restore(),
                b']' => self.mode = Mode::Osc,
                _ => self.mode = Mode::Ground,
            },
            Mode::Csi => match b {
                b'?' => self.private = true,
                b'0'..=b'9' => {
                    self.param = (self.param * 10).saturating_add((b - b'0') as u16).min(9999);
                }
                b';' => self.push_param(),
                0x1b => self.mode = Mode::Esc,
                // VT100 final bytes live in 0x40..=0x7E; digits, `;`, `?`
                // and intermediates are consumed above.
                c if (0x40..=0x7e).contains(&c) => {
                    self.push_param();
                    self.dispatch(c, scr, reply);
                    self.mode = Mode::Ground;
                }
                _ => {} // private/intermediate bytes
            },
            Mode::Osc => {
                if b == 0x07 || b == 0x1b {
                    self.mode = Mode::Ground;
                }
            }
        }
    }

    fn dispatch(&mut self, c: u8, scr: &mut Screen, reply: &mut dyn FnMut(&[u8])) {
        if self.private {
            // `CSI ? <n> h/l`: DEC private modes.
            if (c == b'h' || c == b'l') && self.p0() == 25 {
                scr.show_cursor = c == b'h';
            }
            return;
        }
        match c {
            b'm' => {
                // SGR: every parameter is an independent command; a bare
                // `ESC [ m` resets.
                if self.nparams == 0 {
                    scr.pen_reset();
                } else {
                    for i in 0..self.nparams {
                        scr.sgr(self.params[i]);
                    }
                }
            }
            b'H' | b'f' => {
                let row = if self.nparams == 0 {
                    1
                } else {
                    self.params[0].max(1) as usize
                };
                let col = if self.nparams < 2 {
                    1
                } else {
                    self.params[1].max(1) as usize
                };
                scr.set_cursor(col - 1, row - 1);
            }
            b'A' => scr.move_up(self.p0()),
            b'B' => scr.move_down(self.p0()),
            b'C' => scr.move_forward(self.p0()),
            b'D' => scr.move_back(self.p0()),
            b'G' => scr.set_col(self.p0() - 1),
            b'J' => scr.erase_screen(self.p0().min(3)),
            b'K' => scr.erase_line(self.p0().min(2)),
            b's' => scr.save(),
            b'u' => scr.restore(),
            b'n' => {
                // "CPI 6n": cursor position report, written to the master as
                // a terminal would.
                if self.p0() == 6 {
                    let mut r = [0u8; 32];
                    let s = format!("\x1b[{};{}R", scr.cy + 1, scr.cx + 1);
                    let n = s.len().min(r.len());
                    r[..n].copy_from_slice(&s.as_bytes()[..n]);
                    reply(&r[..n]);
                }
            }
            _ => {}
        }
    }
}

// --- Cell screen -----------------------------------------------------------

#[derive(Clone, Copy)]
struct Cell {
    ch: u8,
    fg: u8,
    bg: u8,
    bold: bool,
    inv: bool,
}

impl Cell {
    fn blank(fg: u8, bg: u8) -> Cell {
        Cell { ch: b' ', fg, bg, bold: false, inv: false }
    }
    fn erase_with(pen: &Pen) -> Cell {
        Cell { ch: b' ', fg: pen.fg, bg: pen.bg, bold: pen.bold, inv: pen.inv }
    }
}

#[derive(Clone, Copy)]
struct Pen {
    fg: u8,
    bg: u8,
    bold: bool,
    inv: bool,
}

struct Screen {
    cols: usize,
    rows: usize,
    cells: Vec<Cell>,
    dirty: Vec<usize>,
    pen: Pen,
    cx: usize,
    cy: usize,
    saved_cx: usize,
    saved_cy: usize,
    show_cursor: bool,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Screen {
        let cells = vec![Cell::blank(DEF_FG, DEF_BG); cols * rows];
        let mut screen = Screen {
            cols,
            rows,
            cells,
            dirty: Vec::new(),
            pen: Pen { fg: DEF_FG, bg: DEF_BG, bold: false, inv: false },
            cx: 0,
            cy: 0,
            saved_cx: 0,
            saved_cy: 0,
            show_cursor: true,
        };
        // The framebuffer is not ours until we own the display, so every cell
        // must be repainted on the first pass to clear whatever the kernel
        // console (or the wizard) left behind.
        screen.mark_all();
        screen
    }

    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.cols + x
    }

    fn set(&mut self, x: usize, y: usize, cell: Cell) {
        if x >= self.cols || y >= self.rows {
            return;
        }
        let i = self.idx(x, y);
        let old = self.cells[i];
        // Skip the store+redraw when nothing changed.
        if old.ch == cell.ch && old.fg == cell.fg && old.bg == cell.bg
            && old.bold == cell.bold && old.inv == cell.inv
        {
            return;
        }
        self.cells[i] = cell;
        self.dirty.push(i);
    }

    fn mark_all(&mut self) {
        self.dirty.clear();
        let n = self.cols * self.rows;
        for i in 0..n {
            self.dirty.push(i);
        }
    }

    fn put(&mut self, ch: u8) {
        self.set(self.cx, self.cy, Cell {
            ch,
            fg: self.pen.fg,
            bg: self.pen.bg,
            bold: self.pen.bold,
            inv: self.pen.inv,
        });
        if self.cx + 1 >= self.cols {
            self.cx = 0;
            self.lf();
        } else {
            self.cx += 1;
        }
    }

    fn cr(&mut self) {
        self.cx = 0;
    }

    fn lf(&mut self) {
        if self.cy + 1 >= self.rows {
            self.scroll();
        } else {
            self.cy += 1;
        }
    }

    fn bs(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        }
        self.set(self.cx, self.cy, Cell::erase_with(&self.pen));
    }

    fn tab(&mut self) {
        let next = (self.cx / 8 + 1) * 8;
        self.cx = next.min(self.cols - 1);
    }

    /// Scroll the whole screen up one row, blanking the bottom line.
    fn scroll(&mut self) {
        let w = self.cols;
        self.cells.copy_within(w.., 0);
        for i in (self.rows - 1) * w..self.rows * w {
            self.cells[i] = Cell::blank(DEF_FG, DEF_BG);
        }
        self.mark_all();
    }

    fn set_cursor(&mut self, x: usize, y: usize) {
        self.cx = x.min(self.cols - 1);
        self.cy = y.min(self.rows - 1);
    }

    fn set_col(&mut self, x: usize) {
        self.cx = x.min(self.cols - 1);
    }

    fn move_up(&mut self, n: usize) {
        self.cy = self.cy.saturating_sub(n);
    }
    fn move_down(&mut self, n: usize) {
        self.cy = (self.cy + n).min(self.rows - 1);
    }
    fn move_forward(&mut self, n: usize) {
        self.cx = (self.cx + n).min(self.cols - 1);
    }
    fn move_back(&mut self, n: usize) {
        self.cx = self.cx.saturating_sub(n);
    }

    fn save(&mut self) {
        self.saved_cx = self.cx;
        self.saved_cy = self.cy;
    }
    fn restore(&mut self) {
        self.cx = self.saved_cx;
        self.cy = self.saved_cy;
    }

    fn erase_line(&mut self, mode: usize) {
        match mode {
            0 => {
                for x in self.cx..self.cols {
                    self.set(x, self.cy, Cell::erase_with(&self.pen));
                }
            }
            1 => {
                for x in 0..=self.cx {
                    self.set(x, self.cy, Cell::erase_with(&self.pen));
                }
            }
            _ => {
                for x in 0..self.cols {
                    self.set(x, self.cy, Cell::erase_with(&self.pen));
                }
            }
        }
    }

    fn erase_screen(&mut self, mode: usize) {
        match mode {
            0 => {
                for x in self.cx..self.cols {
                    self.set(x, self.cy, Cell::erase_with(&self.pen));
                }
                for y in self.cy + 1..self.rows {
                    for x in 0..self.cols {
                        self.set(x, y, Cell::erase_with(&self.pen));
                    }
                }
            }
            1 => {
                for y in 0..=self.cy {
                    for x in 0..self.cols {
                        self.set(x, y, Cell::erase_with(&self.pen));
                    }
                }
            }
            _ => {
                for y in 0..self.rows {
                    for x in 0..self.cols {
                        self.set(x, y, Cell::erase_with(&self.pen));
                    }
                }
                self.cx = 0;
                self.cy = 0;
            }
        }
    }

    fn pen_reset(&mut self) {
        self.pen = Pen { fg: DEF_FG, bg: DEF_BG, bold: false, inv: false };
    }

    fn sgr(&mut self, p: u16) {
        match p {
            0 => self.pen_reset(),
            1 => self.pen.bold = true,
            22 => self.pen.bold = false,
            7 => self.pen.inv = true,
            27 => self.pen.inv = false,
            39 => self.pen.fg = DEF_FG,
            49 => self.pen.bg = DEF_BG,
            30..=37 => self.pen.fg = (p - 30) as u8,
            90..=97 => self.pen.fg = (p - 90) as u8 + 8,
            40..=47 => self.pen.bg = (p - 40) as u8,
            100..=107 => self.pen.bg = (p - 100) as u8 + 8,
            _ => {}
        }
    }

    /// Repaint every dirty cell plus the cursor, then release the list.
    fn paint(&mut self, fb: &Display) {
        for &i in &self.dirty {
            let c = self.cells[i];
            let (fg, bg) = resolve(c.fg, c.bg, c.bold, c.inv);
            fb.put_char(i % self.cols, i / self.cols, c.ch, fg, bg);
        }
        self.dirty.clear();
        if self.show_cursor {
            let c = self.cells[self.idx(self.cx, self.cy)];
            // Inverse-video block cursor over the current cell.
            let (fg, bg) = resolve(c.fg, c.bg, c.bold, true);
            fb.put_char(self.cx, self.cy, c.ch, bg, fg);
        }
    }
}

/// Map an ANSI color code (0-7 as stored by SGR, 8-15 for bright variants)
/// onto this system's VGA palette index. The pen stores the ANSI code; only the
/// rasterizer converts, so SGR colors (30-37/90-97) come out right on a palette
/// whose order is the IBM/CGA one (1=blue, 2=green, 3=cyan, 4=red, 5=magenta,
/// 6=brown, 9=bright blue, 12=bright red, ...).
fn ansi_to_pal(c: u8, bold: bool) -> u8 {
    const NORMAL: [u8; 8] = [0, 4, 2, 6, 1, 5, 3, 7];
    const BRIGHT: [u8; 8] = [8, 12, 10, 14, 9, 13, 11, 15];
    if c >= 8 {
        BRIGHT[(c - 8) as usize]
    } else if bold {
        BRIGHT[c as usize]
    } else {
        NORMAL[c as usize]
    }
}

/// Apply bold brightening and reverse-video before rasterizing.
fn resolve(fg: u8, bg: u8, bold: bool, inv: bool) -> (u8, u8) {
    let (mut fg, mut bg) = (ansi_to_pal(fg, bold), ansi_to_pal(bg, false));
    if inv {
        core::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

// --- main -------------------------------------------------------------------

fn main() -> ! {
    // Take the display away from the kernel console first, so our framebuffer
    // writes are the only ones hitting the screen.
    let _ = syscall::console_detach();

    let fb = match Display::new() {
        Some(fb) => fb,
        None => {
            println!("[term] no framebuffer");
            syscall::proc_exit_code(1);
        }
    };
    let cols = fb.cols();
    let rows = fb.rows();
    println!(
        "[term] framebuffer {}x{} ({}x{} cells)",
        fb.width, fb.height, cols, rows
    );
    let mut screen = Screen::new(cols, rows);

    // The console PTY master: `consoled` writes keystrokes into it, the
    // line discipline echoes and edits them, and the shell's output arrives
    // here translated. Drain any stale bytes the wizard left behind.
    //
    // `/dev/ptmx0`, not `/dev/ptmx`. The master of pair 0 -- the pty whose
    // slave is `SLAVE` below -- is the other end of the *same* terminal this
    // program draws for. `/dev/ptmx` is a multiplexer: opening it allocates a
    // new pair instead, which would leave us reading an empty terminal while
    // keystrokes went to `consoled`'s end of pair 0.
    let master = match syscall::open(MASTER, syscall::O_RDWR, 0) {
        Ok(fd) => fd,
        Err(e) => {
            println!("[term] open {} failed: {}", MASTER, e);
            syscall::proc_exit_code(1);
        }
    };
    drain(master);

    // Advertise our geometry on the slave (TIOCGWINSZ for anyone who asks).
    if let Ok(slave) = syscall::open(SLAVE, syscall::O_RDWR, 0) {
        let mut ws = syscall::Winsize {
            ws_row: rows as u16,
            ws_col: cols as u16,
            ws_xpixel: fb.width as u16,
            ws_ypixel: fb.height as u16,
        };
        let _ = syscall::ioctl(slave, syscall::TIOCSWINSZ, &mut ws);
        let _ = syscall::close(slave);
    }

    println!("[term] spawning shell");
    let _ = syscall::proc_spawn(PROG_SHELL, None);

    screen.paint(&fb);

    let mut parser = Parser::new();
    let mut p = [PollFd {
        fd: master as i32,
        events: poll_events::POLLIN,
        revents: 0,
    }];
    let mut buf = [0u8; 1024];

    loop {
        match syscall::poll(&mut p, 100) {
            Ok(_) => {}
            Err(_) => {
                syscall::yield_now();
                continue;
            }
        }
        if p[0].revents & poll_events::POLLIN != 0 {
            match syscall::read(master, &mut buf) {
                Ok(n) if n > 0 => {
                    parser.feed(&buf[..n], &mut screen, &mut |reply| {
                        let _ = syscall::write(master, reply);
                    });
                }
                _ => {}
            }
            screen.paint(&fb);
        } else {
            // Idle: just refresh the cursor so it stays visible.
            screen.paint(&fb);
        }
    }
}

/// Drop whatever is already buffered on the master (stale echoes).
fn drain(fd: usize) {
    let mut tmp = [0u8; 256];
    loop {
        let mut p = [PollFd {
            fd: fd as i32,
            events: poll_events::POLLIN,
            revents: 0,
        }];
        if syscall::poll(&mut p, 0).unwrap_or(0) == 0 {
            return;
        }
        match syscall::read(fd, &mut tmp) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}