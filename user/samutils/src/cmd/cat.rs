// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! `cat [FILE...]` — concatenate files to the tty (or read the tty itself
//! line by line when invoked without arguments).

use nutcracker_rt::syscall;

use super::{open_read, open_tty_in, read_all, write_all, Tty};

fn dump(fd: usize, out: &mut Tty) {
    let data = read_all(fd);
    write_all(out.fd, &data);
}

pub fn main(args: &[&str], out: &mut Tty) -> i32 {
    if args.is_empty() {
        // Read the tty (canonical lines) and print each one back, like a real
        // tty `cat`; end with Ctrl-D (empty line) in our line discipline.
        let fd = match open_tty_in() {
            Some(fd) => fd,
            None => return 1,
        };
        dump(fd, out);
        let _ = syscall::close(fd);
        return 0;
    }
    let mut rc = 0;
    for p in args {
        match open_read("cat", p, out) {
            Some(fd) => {
                dump(fd, out);
                let _ = syscall::close(fd);
            }
            None => rc = 1,
        }
    }
    rc
}