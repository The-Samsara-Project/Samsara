// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `ls [-l] [PATH]` — list directory entries.

use core::fmt::Write;
use nutcracker_rt::syscall::{self, Stat};

use super::{errstr, Tty};

/// Render one permission triple position: `ch` on set, `-` on clear.
fn perm_bit(mode: u32, offset: u32, ch: char) -> char {
    if mode & offset != 0 {
        ch
    } else {
        '-'
    }
}

fn mode_string(mode: u32, kind: u32) -> [u8; 10] {
    let mut s = [b'-'; 10];
    s[0] = match kind {
        1 => b'd',
        2 => b'c',
        3 => b'p',
        _ => b'-',
    };
    // Owner.
    s[1] = perm_bit(mode, 0o400, 'r') as u8;
    s[2] = perm_bit(mode, 0o200, 'w') as u8;
    s[3] = perm_bit(mode, 0o100, 'x') as u8;
    // Group.
    s[4] = perm_bit(mode, 0o040, 'r') as u8;
    s[5] = perm_bit(mode, 0o020, 'w') as u8;
    s[6] = perm_bit(mode, 0o010, 'x') as u8;
    // Other.
    s[7] = perm_bit(mode, 0o004, 'r') as u8;
    s[8] = perm_bit(mode, 0o002, 'w') as u8;
    s[9] = perm_bit(mode, 0o001, 'x') as u8;
    s
}

/// Split a readdir buffer (NUL-separated names) into names.
fn parse_names(buf: &[u8]) -> alloc::vec::Vec<&[u8]> {
    let mut out = alloc::vec::Vec::new();
    let mut i = 0usize;
    while i < buf.len() {
        let start = i;
        while i < buf.len() && buf[i] != 0 {
            i += 1;
        }
        if i > start {
            out.push(&buf[start..i]);
        }
        i += 1;
    }
    out
}

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    let mut long = false;
    let mut path = ".";
    for a in args {
        if *a == "-l" || *a == "-al" || *a == "-la" {
            long = true;
        } else if *a == "-a" || *a == "-A" {
            // Hidden entries; accepted for compatibility (none exist today).
        } else if a.starts_with('-') {
            let _ = writeln!(out, "ls: unrecognized option '{}'", a);
            return 1;
        } else {
            path = a;
        }
    }

    // A plain file (or device) argument lists just that entry; only
    // directories go through readdir.
    let mut st = Stat {
        mode: 0,
        uid: 0,
        gid: 0,
        kind: 0,
        size: 0,
    };
    if let Ok(()) = syscall::stat(path, &mut st) {
        if st.kind != 1 {
            // kind: 0 file, 1 dir, 2 char, 3 pipe.
            if long {
                let m = mode_string(st.mode, st.kind);
                let _ = writeln!(
                    out,
                    "{} {:>4} {}",
                    core::str::from_utf8(&m).unwrap_or("----------"),
                    st.size,
                    path
                );
            } else {
                let _ = writeln!(out, "{}", path);
            }
            return 0;
        }
    }

    // Query the required buffer size, then fetch the names.
    let need = match syscall::readdir(path, &mut []) {
        Ok(n) => n,
        Err(e) => {
            let _ = writeln!(out, "ls: {}: {}", path, errstr(e));
            return 1;
        }
    };
    let mut buf = alloc::vec![0u8; need];
    match syscall::readdir(path, &mut buf) {
        Ok(_) => {}
        Err(e) => {
            let _ = writeln!(out, "ls: {}: {}", path, errstr(e));
            return 1;
        }
    }
    let names = parse_names(&buf);
    if names.is_empty() {
        let _ = writeln!(out);
        return 0;
    }

    if !long {
        // Space-separated on one (wrapping) line.
        for (i, n) in names.iter().enumerate() {
            if i > 0 {
                let _ = out.write_str("  ");
            }
            let _ = out.write_str(core::str::from_utf8(n).unwrap_or("?"));
        }
        let _ = writeln!(out);
        return 0;
    }

    // Long format: stat every entry as `<path>/<name>`.
    let mut rc = 0;
    for n in &names {
        let name = core::str::from_utf8(n).unwrap_or("?");
        let full;
        let joined;
        let p = if path.ends_with('/') {
            joined = alloc::format!("{}{}", path, name);
            joined.as_str()
        } else {
            full = alloc::format!("{}/{}", path, name);
            full.as_str()
        };
        let mut st = Stat {
            mode: 0,
            uid: 0,
            gid: 0,
            kind: 0,
            size: 0,
        };
        if let Err(e) = syscall::stat(p, &mut st) {
            let _ = writeln!(out, "ls: {}: {}", name, errstr(e));
            rc = 1;
            continue;
        }
        let m = mode_string(st.mode, st.kind);
        let _ = writeln!(
            out,
            "{} {:>4} {}",
            core::str::from_utf8(&m).unwrap_or("----------"),
            st.size,
            name
        );
    }
    rc
}