// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa
//
// Console server (endpoint 2), a boot-time server in `user/servers/`. It is
// the single consumer of the serial/console line in user space: other servers
// please it by IPC instead of poking DEBUG_WRITE directly. It answers `Call`s
// with "ok", logs `Notify`s, reports `Irq` events, and — when the notify
// comes from the keyboard server — feeds the bytes into the native terminal's
// PTY master so the shell on `/dev/pts0` receives them.

#![no_std]
#![no_main]

use nutcracker_rt::ipc::{self, MsgFrame};
use nutcracker_rt::{println, syscall};

/// The master end of pair 0, the pty whose slave is `/dev/pts0`.
///
/// Deliberately not `/dev/ptmx`: that name is a multiplexer which allocates a
/// fresh, unrelated pair on every open. See the comment at the open below.
const MASTER: &str = "/dev/ptmx0";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    println!("[consoled] console server online");

    // The keyboard driver forwards decoded keys here; we feed them into the
    // native terminal's PTY master so the shell on `/dev/pts0` receives them.
    //
    // `/dev/ptmx0`, NOT `/dev/ptmx`. This is the distinction that decides whether
    // the installer works at all:
    //
    //   - `/dev/pts0` is pair 0, the boot console, published under a fixed name
    //     because the installer and terminal open that path by name.
    //   - `/dev/ptmx0` is that pair's master -- the other end of *that* pty.
    //   - `/dev/ptmx` is a multiplexer. Opening it does not give you pair 0;
    //     it allocates a brand-new pair and registers it as `/dev/pts1`, etc.
    //
    // So opening the multiplexer here handed every keystroke to an orphan
    // terminal that nothing ever reads, while the installer sat waiting on
    // `pts0` forever. The failure looked like a frozen installer, and because
    // this used `.ok()` it still logged a line per keypress, which made the
    // input path look alive.
    let master = match syscall::open(MASTER, syscall::O_RDWR, 0) {
        Ok(fd) => Some(fd),
        Err(e) => {
            // Loud, because from here on every keystroke is going nowhere.
            println!("[consoled] open {} failed: {}; no key will reach the shell", MASTER, e);
            None
        }
    };

    let mut frame = MsgFrame::new();
    loop {
        if let Err(e) = ipc::recv(&mut frame) {
            println!("[consoled] recv error: {}", e);
            continue;
        }
        match frame.msg_kind() {
            ipc::Kind::Call => {
                let text = core::str::from_utf8(frame.payload()).unwrap_or("<binary>");
                println!("[consoled] call #{} from {}: {}", frame.call_id, frame.from, text);
                let mut rep = MsgFrame::new();
                rep.set_payload(b"ok");
                if let Err(e) = ipc::reply(frame.from as u64, frame.call_id, &rep) {
                    println!("[consoled] reply failed: {}", e);
                }
            }
            ipc::Kind::Notify => {
                // Keystrokes from the keyboard driver (endpoint 3) are routed to
                // the native terminal: `/dev/ptmx` is the master, the shell owns
                // `/dev/pts0`. Anything else is logged as before.
                if frame.from as u64 == syscall::EP_INPUTD {
                    if let Some(fd) = master {
                        println!("[consoled] key @{}", syscall::uptime_ms());
                        let _ = syscall::write(fd, frame.payload());
                        continue;
                    }
                }
                let text = core::str::from_utf8(frame.payload()).unwrap_or("<binary>");
                println!("[consoled] from {}: {}", frame.from, text);
            }
            ipc::Kind::Irq => {
                println!("[consoled] hardware interrupt irq={}", frame.tag);
            }
            ipc::Kind::Reply => {
                println!("[consoled] stray reply #{}", frame.call_id);
            }
        }
    }
}