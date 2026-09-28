// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Keyboard driver (endpoint 3), a boot-time server in `user/servers/`. Owns
// IRQ 1 and I/O ports 0x60/0x64 - the first hardware actually driven from
// user space. Work arrives as `Irq` IPC messages; decoded characters are
// forwarded to the console server as `Notify` frames. The kernel initialized
// the controller at boot (interrupts armed, scancode set 1 via translation).

#![no_std]
#![no_main]

use core::arch::asm;

use nutcracker_rt::ipc::{self, MsgFrame};
use nutcracker_rt::println;
use nutcracker_rt::syscall::{self, EP_CONSOLED};

const DATA_PORT: u16 = 0x60;
const STATUS_PORT: u16 = 0x64;
const IRQ_KBD: u64 = 1;
const IRQ_MOUSE: u64 = 12;

/// "No such file or directory", as the kernel reports it. Spelled out because
/// this is a `no_std` binary with no libc to borrow the name from, and the
/// distinction matters: this one error means "not configured yet" and is
/// expected, while any other means the configuration is present and being
/// ignored.
const ENOENT: i64 = -2;

/// Read one byte from an I/O port.
///
/// # Safety
/// The port must have been granted to this process via `PORT_ALLOW`.
unsafe fn inb(port: u16) -> u8 {
    let v: u8;
    asm!(
        "in al, dx",
        out("al") v,
        in("dx") port,
        options(nostack, preserves_flags),
    );
    v
}

fn status() -> u8 {
    // SAFETY: 0x64 is granted to this driver.
    unsafe { inb(STATUS_PORT) }
}

/// Read one byte from the output buffer. Returns `(byte, is_aux)` so the
/// keyboard path and the mouse path can be routed separately once this driver
/// owns both IRQ 1 (keyboard) and IRQ 12 (mouse).
fn read_data() -> Option<(u8, bool)> {
    // Bit 0 = output buffer full; bit 5 = aux (mouse) data.
    let s = status();
    if s & 0x01 == 0 {
        return None;
    }
    let is_aux = s & 0x20 != 0;
    // SAFETY: 0x60 is granted to this driver.
    Some((unsafe { inb(DATA_PORT) }, is_aux))
}

/// Discard every byte currently waiting in the output buffer (used right
/// before binding IRQ 12 so a stale mouse byte cannot wedge the edge).
fn flush_output() {
    while status() & 0x01 != 0 {
        // SAFETY: 0x60 is granted to this driver.
        let _ = unsafe { inb(DATA_PORT) };
    }
}

/// One selectable layout: a 47-entry `(plain, shifted)` table in a fixed
/// scancode order (0x02..=0x0D, 0x10..=0x1B, 0x1E..=0x29, 0x2B, 0x2C..=0x35).
///
/// Only ASCII is expressible here — the framebuffer console font has no
/// Latin-1 glyphs — so the non-US variants remap letters and the punctuation
/// that stays in ASCII.
struct Layout {
    name: &'static str,
    map: &'static [(u8, u8)],
}

/// US QWERTY — the default.
const US_MAP: &[(u8, u8)] = &[
    (b'1', b'!'), // 0x02
    (b'2', b'@'),
    (b'3', b'#'),
    (b'4', b'$'),
    (b'5', b'%'),
    (b'6', b'^'),
    (b'7', b'&'),
    (b'8', b'*'),
    (b'9', b'('),
    (b'0', b')'), // 0x0B
    (b'-', b'_'), // 0x0C
    (b'=', b'+'), // 0x0D
    (b'q', b'Q'), // 0x10
    (b'w', b'W'),
    (b'e', b'E'),
    (b'r', b'R'),
    (b't', b'T'),
    (b'y', b'Y'),
    (b'u', b'U'),
    (b'i', b'I'),
    (b'o', b'O'),
    (b'p', b'P'), // 0x19
    (b'[', b'{'), // 0x1A
    (b']', b'}'), // 0x1B
    (b'a', b'A'), // 0x1E
    (b's', b'S'),
    (b'd', b'D'),
    (b'f', b'F'),
    (b'g', b'G'),
    (b'h', b'H'),
    (b'j', b'J'),
    (b'k', b'K'),
    (b'l', b'L'),  // 0x26
    (b';', b':'),  // 0x27
    (b'\'', b'"'), // 0x28
    (b'`', b'~'),  // 0x29
    (b'\\', b'|'), // 0x2B
    (b'z', b'Z'),  // 0x2C
    (b'x', b'X'),
    (b'c', b'C'),
    (b'v', b'V'),
    (b'b', b'B'),
    (b'n', b'N'),
    (b'm', b'M'), // 0x32
    (b',', b'<'), // 0x33
    (b'.', b'>'), // 0x34
    (b'/', b'?'), // 0x35
];

/// German QWERTZ (ASCII subset): Y and Z trade places; every other key is
/// US-identical (the real German row punctuation is non-ASCII and the console
/// font cannot render it).
const DE_MAP: &[(u8, u8)] = &[
    (b'1', b'!'), (b'2', b'@'), (b'3', b'#'), (b'4', b'$'), (b'5', b'%'),
    (b'6', b'^'), (b'7', b'&'), (b'8', b'*'), (b'9', b'('), (b'0', b')'),
    (b'-', b'_'), (b'=', b'+'),
    (b'q', b'Q'), (b'w', b'W'), (b'e', b'E'), (b'r', b'R'), (b't', b'T'),
    (b'z', b'Z'), // Y key types Z
    (b'u', b'U'), (b'i', b'I'), (b'o', b'O'), (b'p', b'P'),
    (b'[', b'{'), (b']', b'}'),
    (b'a', b'A'), (b's', b'S'), (b'd', b'D'), (b'f', b'F'), (b'g', b'G'),
    (b'h', b'H'), (b'j', b'J'), (b'k', b'K'), (b'l', b'L'),
    (b';', b':'), (b'\'', b'"'), (b'`', b'~'),
    (b'\\', b'|'),
    (b'y', b'Y'), // Z key types Y
    (b'x', b'X'), (b'c', b'C'), (b'v', b'V'), (b'b', b'B'), (b'n', b'N'),
    (b'm', b'M'), (b',', b'<'), (b'.', b'>'), (b'/', b'?'),
];

/// Colemak: home-row-heavy remap, punctuation unchanged.
const COLEMAK_MAP: &[(u8, u8)] = &[
    (b'1', b'!'), (b'2', b'@'), (b'3', b'#'), (b'4', b'$'), (b'5', b'%'),
    (b'6', b'^'), (b'7', b'&'), (b'8', b'*'), (b'9', b'('), (b'0', b')'),
    (b'-', b'_'), (b'=', b'+'),
    (b'q', b'Q'), (b'w', b'W'), (b'f', b'F'), (b'p', b'P'), (b'g', b'G'),
    (b'j', b'J'), (b'l', b'L'), (b'u', b'U'), (b'y', b'Y'), (b';', b':'),
    (b'[', b'{'), (b']', b'}'),
    (b'a', b'A'), (b'r', b'R'), (b's', b'S'), (b't', b'T'), (b'd', b'D'),
    (b'h', b'H'), (b'n', b'N'), (b'e', b'E'), (b'i', b'I'), (b'o', b'O'),
    (b'\'', b'"'), (b'`', b'~'),
    (b'\\', b'|'),
    (b'z', b'Z'), (b'x', b'X'), (b'c', b'C'), (b'v', b'V'), (b'b', b'B'),
    (b'k', b'K'), (b'm', b'M'), (b',', b'<'), (b'.', b'>'), (b'/', b'?'),
];

/// US Dvorak.
const DVORAK_MAP: &[(u8, u8)] = &[
    (b'1', b'!'), (b'2', b'@'), (b'3', b'#'), (b'4', b'$'), (b'5', b'%'),
    (b'6', b'^'), (b'7', b'&'), (b'8', b'*'), (b'9', b'('), (b'0', b')'),
    (b'[', b'{'), (b']', b'}'),
    (b'\'', b'"'), (b',', b'<'), (b'.', b'>'), (b'p', b'P'), (b'y', b'Y'),
    (b'f', b'F'), (b'g', b'G'), (b'c', b'C'), (b'r', b'R'), (b'l', b'L'),
    (b'/', b'?'), (b'=', b'+'),
    (b'a', b'A'), (b'o', b'O'), (b'e', b'E'), (b'u', b'U'), (b'i', b'I'),
    (b'd', b'D'), (b'h', b'H'), (b't', b'T'), (b'n', b'N'), (b's', b'S'),
    (b'-', b'_'), (b';', b':'),
    (b'\\', b'|'),
    (b';', b':'), (b'q', b'Q'), (b'j', b'J'), (b'k', b'K'), (b'x', b'X'),
    (b'b', b'B'), (b'm', b'M'), (b'w', b'W'), (b'v', b'V'), (b'z', b'Z'),
];

/// The layouts offered by the installer, in the same order it presents them.
const LAYOUTS: [Layout; 4] = [
    Layout { name: "us", map: US_MAP },
    Layout { name: "de", map: DE_MAP },
    Layout { name: "colemak", map: COLEMAK_MAP },
    Layout { name: "dvorak", map: DVORAK_MAP },
];

fn map_key(make: u8, layout: usize) -> Option<(u8, u8)> {
    let map = LAYOUTS.get(layout)?.map;
    match make {
        0x02..=0x0D => map.get((make - 0x02) as usize).copied(),
        0x10..=0x1B => map.get((make - 0x10 + 12) as usize).copied(),
        0x1E..=0x29 => map.get((make - 0x1E + 24) as usize).copied(),
        0x2B => map.get(36).copied(),
        0x2C..=0x35 => map.get((make - 0x2C + 37) as usize).copied(),
        _ => None,
    }
}

/// Raw key state for every decoded key: the make code (0xE0 prefix is folded
/// into `extended`), the make/break flag and a snapshot of the modifier mask
/// at the moment the byte was processed. These fields are the `ipc::inp::KEY`
/// wire layout, kept as the decoded form for any future consumer; characters
/// still reach the console through `payload`.
#[allow(dead_code)]
struct RawKey {
    make: u8,
    extended: bool,
    down: bool,
    mods: u64,
}

struct Kbd {
    shift: bool,
    ctrl: bool,
    alt: bool,
    caps: bool,
    e0: bool,
    /// Index into [`LAYOUTS`] for the active keymap.
    layout: usize,
}

impl Kbd {
    const fn new() -> Self {
        Kbd {
            shift: false,
            ctrl: false,
            alt: false,
            caps: false,
            e0: false,
            layout: 0,
        }
    }

    /// Current modifier mask (mirrors `ipc::MOD_*`).
    fn mods(&self) -> u64 {
        let mut m: u64 = 0;
        if self.shift {
            m |= ipc::MOD_SHIFT;
        }
        if self.ctrl {
            m |= ipc::MOD_CTRL;
        }
        if self.alt {
            m |= ipc::MOD_ALT;
        }
        if self.caps {
            m |= ipc::MOD_CAPS;
        }
        m
    }

    fn handle(&mut self, code: u8) -> Option<([u8; 4], usize, RawKey)> {
        if code == 0xE0 {
            self.e0 = true;
            return None;
        }
        let extended = core::mem::replace(&mut self.e0, false);
        let is_break = code & 0x80 != 0;
        let make = code & 0x7F;
        let raw = RawKey {
            make,
            extended,
            down: !is_break,
            mods: self.mods(),
        };

        match make {
            0x2A | 0x36 if !extended => {
                self.shift = !is_break;
                return None;
            }
            0x1D => {
                self.ctrl = !is_break;
                return None;
            }
            0x38 => {
                self.alt = !is_break;
                return None;
            }
            0x3A if !extended && !is_break => {
                self.caps = !self.caps;
                return None;
            }
            _ => {}
        }
        if is_break {
            return None;
        }

        let mut out = [0u8; 4];
        let n = match (extended, make) {
            (true, 0x48) => {
                out[..3].copy_from_slice(b"\x1b[A");
                3
            }
            (true, 0x50) => {
                out[..3].copy_from_slice(b"\x1b[B");
                3
            }
            (true, 0x4B) => {
                out[..3].copy_from_slice(b"\x1b[D");
                3
            }
            (true, 0x4D) => {
                out[..3].copy_from_slice(b"\x1b[C");
                3
            }
            (true, 0x53) => {
                out[..4].copy_from_slice(b"\x1b[3~");
                4
            }
            (false, 0x01) => {
                out[0] = 0x1B;
                1
            }
            (false, 0x39) => {
                out[0] = b' ';
                1
            }
            (false, 0x1C) => {
                out[0] = b'\n';
                1
            }
            (false, 0x0E) => {
                out[0] = 0x08;
                1
            }
            (false, 0x0F) => {
                out[0] = b'\t';
                1
            }
            (false, _) => match map_key(make, self.layout) {
                Some((plain, shifted)) => {
                    let mut c = if self.shift { shifted } else { plain };
                    if self.caps && c.is_ascii_alphabetic() {
                        c = if self.shift {
                            c.to_ascii_lowercase()
                        } else {
                            c.to_ascii_uppercase()
                        };
                    }
                    out[0] = c;
                    1
                }
                None => return None,
            },
            _ => return None,
        };
        Some((out, n, raw))
    }
}

static mut KBD: Kbd = Kbd::new();

/// PS/2 auxiliary-channel packet decoder. Standard 3-byte protocol, same as
/// the kernel's legacy mouse driver: overflow bits drop the packet, X/Y are
/// 9-bit two's complement deltas, and Y is inverted so a positive delta means
/// *down* on screen.
struct Mouse {
    pkt: [u8; 3],
    len: usize,
}

impl Mouse {
    const fn new() -> Self {
        Mouse { pkt: [0; 3], len: 0 }
    }

    /// Feed one aux byte; returns `(buttons, dx, dy)` once a whole packet
    /// arrived. Buttons: bit0 left, bit1 right, bit2 middle.
    fn push(&mut self, b: u8) -> Option<(u32, i32, i32)> {
        // Byte 0 must have bit 3 set; resynchronize otherwise.
        if self.len == 0 && b & 0x08 == 0 {
            return None;
        }
        self.pkt[self.len] = b;
        self.len += 1;
        if self.len < 3 {
            return None;
        }
        let pkt = self.pkt;
        self.len = 0;
        // X/Y overflow: drop the flawed packet rather than propagate it.
        if pkt[0] & 0xC0 != 0 {
            return None;
        }
        let neg_x = pkt[0] & 0x10 != 0;
        let neg_y = pkt[0] & 0x20 != 0;
        let x = if neg_x { (pkt[1] as i32) - 256 } else { pkt[1] as i32 };
        let y = if neg_y { (pkt[2] as i32) - 256 } else { pkt[2] as i32 };
        Some(((pkt[0] & 0x07) as u32, x, -y))
    }
}

/// Where the installer persists the active layout name (`/etc/keymap.conf`).
const KEYMAP_CONF: &str = "/etc/keymap.conf";

/// A `Notify` arrived with `args[0] == RELOAD_LAYOUT` after the file was
/// rewritten; we re-read it so the file stays the single source of truth.
const RELOAD_LAYOUT: u64 = 1;

/// Read a small text file into `buf`; returns the number of bytes read.
fn read_file(path: &str, buf: &mut [u8]) -> Result<usize, i64> {
    let fd = syscall::open(path, syscall::O_RDONLY, 0)?;
    let r = syscall::read(fd, buf);
    let _ = syscall::close(fd);
    r
}

/// (Re)read `/etc/keymap.conf` and switch the decoder to the named layout.
/// The file holds one token, e.g. `us\n` or `dvorak`.
fn reload_keymap() {
    let mut raw = [0u8; 64];
    let n = match read_file(KEYMAP_CONF, &mut raw) {
        Ok(0) => return,
        Ok(n) => n.min(raw.len()),
        // A missing file is the normal state of a system that has not been
        // configured yet, and the default layout is already active, so saying
        // so on every boot would make an ordinary first boot look broken.
        // Silent: there is nothing for the user to do about it.
        //
        // Any *other* failure is not normal and is worth reporting -- a file
        // that exists but cannot be read means the configuration is present
        // and not being honoured, which silently ignores the user's choice.
        Err(e) if e == -ENOENT => return,
        Err(e) => {
            println!("[inputd] keymap: cannot read {}: {}", KEYMAP_CONF, e);
            return;
        }
    };
    let name = core::str::from_utf8(&raw[..n])
        .unwrap_or("")
        .split_whitespace()
        .next()
        .unwrap_or("");
    let l = LAYOUTS.iter().position(|x| x.name == name);
    match l {
        Some(idx) => {
            // SAFETY: single-threaded driver; only this process touches `KBD`.
            unsafe { (*core::ptr::addr_of_mut!(KBD)).layout = idx };
            println!("[inputd] keymap set to '{}'", name);
        }
        None => println!("[inputd] keymap: unknown layout '{}'", name),
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let epid = syscall::get_epid();
    println!("[inputd] keyboard driver online (endpoint {})", epid);

    if let Err(e) = syscall::port_allow(DATA_PORT, DATA_PORT) {
        println!("[inputd] port 0x60 grant failed: {}", e);
        syscall::proc_exit();
    }
    if let Err(e) = syscall::port_allow(STATUS_PORT, STATUS_PORT) {
        println!("[inputd] port 0x64 grant failed: {}", e);
        syscall::proc_exit();
    }
    // The controller was initialized and interrupt lines armed by the kernel
    // at boot; we merely claim the IRQ routing.
    if let Err(e) = syscall::irq_bind(IRQ_KBD) {
        println!("[inputd] irq 1 bind failed: {}", e);
        syscall::proc_exit();
    }
    println!("[inputd] waiting for key events on irq {}", IRQ_KBD);

    // Claim the auxiliary channel too (the kernel's legacy PS/2 mouse handler
    // steps aside once a user endpoint binds the line; the device is already
    // streaming from boot). Flush stale bytes first so a leftover mouse byte
    // cannot hold the edge asserted and swallow later packets.
    flush_output();
    if let Err(e) = syscall::irq_bind(IRQ_MOUSE) {
        println!("[inputd] irq 12 bind failed: {}", e);
        syscall::proc_exit();
    }
    println!("[inputd] mouse on irq {}", IRQ_MOUSE);

    // Honour a layout configured before this driver came up (e.g. persisted
    // by a previous session's installer).
    reload_keymap();

    let mut frame = MsgFrame::new();
    let mut mouse = Mouse::new();
    loop {
        if let Err(e) = ipc::recv(&mut frame) {
            println!("[inputd] recv error: {}", e);
            continue;
        }
        match frame.msg_kind() {
            ipc::Kind::Irq
                if frame.tag == IRQ_KBD as u32 || frame.tag == IRQ_MOUSE as u32 => {}
            // The installer asks us to pick up a new /etc/keymap.conf.
            ipc::Kind::Notify if frame.args[0] == RELOAD_LAYOUT => {
                reload_keymap();
                continue;
            }
            _ => continue,
        }
        // SAFETY: single-threaded driver; state kept in `KBD`.
        let kbd = unsafe { &mut *core::ptr::addr_of_mut!(KBD) };
        // Drain both channels on every interrupt: the i8042 serializes its
        // output buffer, so bytes from either device may be waiting.
        while let Some((byte, is_aux)) = read_data() {
            if is_aux {
                if let Some((buttons, dx, dy)) = mouse.push(byte) {
                    println!("[inputd] mouse btns {} dx {} dy {}", buttons, dx, dy);
                }
            } else {
                println!("[inputd] scan {:#04x}", byte);
                if let Some((payload, n, _raw)) = kbd.handle(byte) {
                    let mut out = MsgFrame::new();
                    out.set_payload(&payload[..n]);
                    match ipc::send(EP_CONSOLED, &out) {
                        Ok(()) => println!("[inputd] fwd {:?}", &payload[..n]),
                        Err(e) => println!("[inputd] forward to consoled failed: {}", e),
                    }
                }
            }
        }
    }
}