// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! PTY (Pseudo-Terminal) driver for Samsara.
//!
//! Implements Unix98-style pseudo-terminals with master/slave pairs.
//! The master side is controlled by a process (terminal emulator, sshd, etc.)
//! The slave side behaves like a real terminal device.

#![no_std]
#![allow(missing_docs)]

extern crate alloc;

use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::string::String;
use spin::Mutex;

use driver_common::{
    DriverError, DriverResult, CharDevice, DeviceInfo, DeviceClass, DeviceCapabilities,
    DriverRegistration, DriverEntry, register_char_device,
};

mod ldisc;
pub use ldisc::LineDiscipline;
// The termios flag bits are re-exported so `Default for Termios` below can
// spell the conventional terminal state symbolically rather than as magic
// numbers that can be transcribed wrongly.
pub use ldisc::{
    ECHO, ECHOE, ECHOKE, ICANON, ICRNL, IEXTEN, ISIG, IXON, OPOST, ONLCR, VDISCARD, VEOF, VEOL,
    VEOL2, VERASE, VINTR, VKILL, VLNEXT, VMIN, VQUIT, VREPRINT, VSUSP, VSTART, VSTOP, VSWTC,
    VTIME, VWERASE,
};

/// Kernel bridge (exported as `extern "Rust"` from `kernel/src/lib.rs`):
/// deliver a TTY signal to a foreground process group. No-op for pgid 0.
extern "Rust" {
    fn tty_signal(pgid: u32, sig: u32);
}

/// Kernel bridge: the calling task's id, or 0 when there is no current user
/// task (a boot-time context, where registering a waiter is meaningless).
extern "Rust" {
    fn tty_current_task() -> u32;
}

/// Kernel bridge: make `task` runnable. Used to release a task parked on a
/// terminal read once input (or an end-of-file) is available.
extern "Rust" {
    fn tty_wake(task: u32);
}

/// Kernel bridge: may the calling task hand its terminal's foreground role to
/// process group `pgid`? Enforces the POSIX rule that the group must belong to
/// the calling task's session. Returns 1 when allowed.
extern "Rust" {
    fn tty_check_pgrp(pgid: u64) -> bool;
}

/// Set of task ids waiting for readiness on one direction of a terminal.
///
/// Samsara blocks threads on task ids rather than on futures, so the pty
/// tracks waiters explicitly. Registration happens under the same lock that
/// guards the readiness flag, so an event landing between the caller's
/// availability check and its registration cannot be missed.
#[derive(Default)]
struct Waiters {
    tasks: Mutex<Vec<u32>>,
}

impl Waiters {
    const fn new() -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
        }
    }

    /// Register the current task. Returns `true` if it is already waiting,
    /// which lets a caller treat the call as a no-op.
    fn register(&self, task: u32) {
        if task == 0 {
            return;
        }
        let mut t = self.tasks.lock();
        if !t.contains(&task) {
            t.push(task);
        }
    }

    /// Drop a registration; idempotent.
    fn unregister(&self, task: u32) {
        if task == 0 {
            return;
        }
        self.tasks.lock().retain(|t| *t != task);
    }

    /// Wake everyone registered and clear the list, so a task that loops back
    /// around has to re-register rather than spinning on a stale entry.
    fn wake_all(&self) {
        let ids = {
            let mut t = self.tasks.lock();
            core::mem::take(&mut *t)
        };
        for id in ids {
            // SAFETY: the kernel exports this symbol (kernel/src/lib.rs).
            unsafe { tty_wake(id) };
        }
    }

    fn is_empty(&self) -> bool {
        self.tasks.lock().is_empty()
    }
}

/// Maximum PTY pairs supported
const MAX_PTYS: u32 = 256;
const BUFFER_SIZE: usize = 4096;

/// PTY master device - controlled by terminal emulator
pub struct PtyMaster {
    id: u32,
    slave: Arc<PtySlave>,
    input_buffer: Mutex<VecDeque<u8>>,
    output_buffer: Mutex<VecDeque<u8>>,
    packet_mode: AtomicBool,
    closed: AtomicBool,
    /// Tasks parked waiting for the master to become readable (the slave side
    /// produced output). Registered by `poll_park` on the master device.
    read_waiters: Waiters,
}

/// PTY slave device - behaves like a real terminal
pub struct PtySlave {
    id: u32,
    master: Mutex<Option<Arc<PtyMaster>>>,
    /// TTY line discipline: termios state + canonical/raw input buffer.
    /// Data typed on the master flows through here before slave readers see
    /// it (see `ldisc.rs`); echoes are republished on the master.
    ldisc: Mutex<LineDiscipline>,
    winsize: Mutex<Winsize>,
    foreground_pgid: AtomicU32,
    /// Session that owns this terminal, recorded when it is first opened.
    /// `TIOCGSID` reports it; zero means "no controlling session".
    session: AtomicU32,
    closed: AtomicBool,
    /// Tasks parked waiting for input on this terminal. A blocking `read` on
    /// the slave registers here and is released by the master (or by `^D`
    /// making an end-of-file visible).
    read_waiters: Waiters,
}

/// Terminal I/O settings (termios)
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 32],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

impl Default for Termios {
    fn default() -> Self {
        // The conventional POSIX terminal state: canonical input with editing,
        // signals from the keyboard, output post-processing, and software flow
        // control. A program that never calls `tcgetattr` inherits this, so
        // every value here is one a real terminal presents.
        Self {
            c_iflag: ICRNL | IXON,
            c_oflag: OPOST | ONLCR,
            c_cflag: 0x0000_00CB, // B38400 | CS8 | CREAD | HUPCL
            // IEXTEN gates VLNEXT/VWERASE/VREPRINT/VDISCARD. Leaving it clear
            // silently disables ^V, ^W, ^R and ^O, which is what a terminal
            // must not do by default.
            c_lflag: ISIG | ICANON | ECHO | ECHOE | ECHOKE | IEXTEN,
            c_line: 0,
            c_cc: {
                let mut cc = [0u8; 32];
                cc[VINTR] = 3; // ^C
                cc[VQUIT] = 28; // ^backslash
                cc[VERASE] = 127; // DEL
                cc[VKILL] = 21; // ^U
                cc[VEOF] = 4; // ^D
                cc[VTIME] = 0;
                cc[VMIN] = 1;
                cc[VSWTC] = 0;
                cc[VSTART] = 17; // ^Q resumes output
                cc[VSTOP] = 19; // ^S stops output
                cc[VSUSP] = 26; // ^Z
                cc[VEOL] = 0;
                cc[VREPRINT] = 18; // ^R
                cc[VDISCARD] = 15; // ^O
                cc[VWERASE] = 23; // ^W
                cc[VLNEXT] = 22; // ^V quotes the next character
                cc[VEOL2] = 0;
                cc
            },
            c_ispeed: 38400,
            c_ospeed: 38400,
        }
    }
}

/// Window size
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// Global PTY manager
static PTY_MANAGER: Mutex<Option<PtyManager>> = Mutex::new(None);

struct PtyManager {
    next_id: AtomicU32,
    masters: alloc::collections::BTreeMap<u32, Arc<PtyMaster>>,
    slaves: alloc::collections::BTreeMap<u32, Arc<PtySlave>>,
}

impl PtyManager {
    fn new() -> Self {
        Self {
            next_id: AtomicU32::new(0),
            masters: alloc::collections::BTreeMap::new(),
            slaves: alloc::collections::BTreeMap::new(),
        }
    }
    
    fn allocate(&mut self) -> DriverResult<(Arc<PtyMaster>, Arc<PtySlave>)> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if id >= MAX_PTYS {
            return Err(DriverError::OutOfMemory);
        }
        
        let slave = Arc::new(PtySlave {
            id,
            master: Mutex::new(None),
            ldisc: Mutex::new(LineDiscipline::new()),
            winsize: Mutex::new(Winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 }),
            foreground_pgid: AtomicU32::new(0),
            session: AtomicU32::new(0),
            closed: AtomicBool::new(false),
            read_waiters: Waiters::new(),
        });
        
        let master = Arc::new(PtyMaster {
            id,
            slave: slave.clone(),
            input_buffer: Mutex::new(VecDeque::with_capacity(BUFFER_SIZE)),
            output_buffer: Mutex::new(VecDeque::with_capacity(BUFFER_SIZE)),
            packet_mode: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            read_waiters: Waiters::new(),
        });
        
        *slave.master.lock() = Some(master.clone());
        
        self.masters.insert(id, master.clone());
        self.slaves.insert(id, slave.clone());
        
        Ok((master, slave))
    }
    
    fn free(&mut self, id: u32) {
        self.masters.remove(&id);
        self.slaves.remove(&id);
    }
    
    fn get_master(&self, id: u32) -> Option<Arc<PtyMaster>> {
        self.masters.get(&id).cloned()
    }
    
    fn get_slave(&self, id: u32) -> Option<Arc<PtySlave>> {
        self.slaves.get(&id).cloned()
    }
}

/// Initialize PTY subsystem and register the first pair in devfs.
pub fn init() {
    *PTY_MANAGER.lock() = Some(PtyManager::new());
    if let Err(e) = register_devices() {
        driver_common::kerror!("pty: failed to register devices: {:?}", e);
    }
}

/// Allocate a new PTY pair
pub fn allocate() -> DriverResult<(Arc<PtyMaster>, Arc<PtySlave>)> {
    PTY_MANAGER.lock().as_mut().unwrap().allocate()
}

/// Get master by ID
pub fn get_master(id: u32) -> Option<Arc<PtyMaster>> {
    PTY_MANAGER.lock().as_ref().and_then(|m| m.get_master(id))
}

/// Get slave by ID
pub fn get_slave(id: u32) -> Option<Arc<PtySlave>> {
    PTY_MANAGER.lock().as_ref().and_then(|m| m.get_slave(id))
}

/// Terminal ioctl request numbers.
///
/// The `_IO`/`_IOR`/`_IOW` encoding is Linux's, so a program that computes a
/// request from its own `<termios.h>` lands on the same value. Sizes are
/// ignored by the dispatcher, which is why the BSD `A` variants and Linux's
/// `S` variants can share a number space.
pub mod ioctl {
    /// Get terminal attributes.
    pub const TCGETS: u32 = 0x5401;
    /// Set terminal attributes immediately.
    pub const TCSETS: u32 = 0x5402;
    /// Set attributes after output drains.
    pub const TCSETSW: u32 = 0x5403;
    /// Set attributes after input drains.
    pub const TCSETSF: u32 = 0x5404;
    /// Get attributes, BSD spelling.
    pub const TCGETA: u32 = 0x5405;
    /// Set attributes immediately, BSD spelling.
    pub const TCSETA: u32 = 0x5406;
    pub const TCSETAW: u32 = 0x5407;
    pub const TCSETAF: u32 = 0x5408;
    /// Send a break.
    pub const TCSBRK: u32 = 0x5409;
    /// Start/stop/flush terminal output and input.
    pub const TCXONC: u32 = 0x540A;
    /// Discard buffered input and/or output.
    pub const TCFLSH: u32 = 0x540B;
    /// Get the foreground process group.
    pub const TIOCGPGRP: u32 = 0x540F;
    /// Set the foreground process group.
    pub const TIOCSPGRP: u32 = 0x5410;
    /// Get/set the window size.
    pub const TIOCGWINSZ: u32 = 0x5413;
    pub const TIOCSWINSZ: u32 = 0x5414;
    /// Number of bytes available to read.
    pub const FIONREAD: u32 = 0x541B;
    /// Get the session id of this terminal.
    pub const TIOCGSID: u32 = 0x5429;
    /// Get the pty pair index (Linux).
    pub const TIOCGPTN: u32 = 0x80045430;
    /// Lock/unlock the pty slave (Linux).
    pub const TIOCSPTLCK: u32 = 0x40045431;
}

impl PtyMaster {
    /// Get PTY number
    pub fn id(&self) -> u32 {
        self.id
    }
    
    /// Get slave name (e.g., "pts/0")
    pub fn slave_name(&self) -> String {
        alloc::format!("pts/{}", self.id)
    }
    
    /// Read from master (data from slave)
    pub fn read(&self, buf: &mut [u8]) -> usize {
        if self.closed.load(Ordering::Acquire) {
            return 0;
        }
        
        let mut input = self.input_buffer.lock();
        let mut n = 0;
        while n < buf.len() {
            if let Some(byte) = input.pop_front() {
                buf[n] = byte;
                n += 1;
            } else {
                break;
            }
        }
        n
    }
    
    /// Write to master (data to slave). Bytes are pushed through the slave's
    /// line discipline: they are translated, edited, echoed and (in canonical
    /// mode) delivered to slave readers only once a line completes. Echo
    /// bytes are republished on the master so a terminal emulator can draw
    /// what the user types.
    pub fn write(&self, buf: &[u8]) -> usize {
        if self.closed.load(Ordering::Acquire) {
            return 0;
        }

        // Note: output being stopped does *not* stop input. `^S` halts what the
        // terminal draws and what the slave writes, but the byte stream keeps
        // being processed — in particular `^Q` has to reach the discipline to
        // lift the stop. Discarding input here would leave the terminal stuck
        // with no way out.

        let mut echo = alloc::vec::Vec::new();
        let mut signal = None;
        {
            let mut ldisc = self.slave.ldisc.lock();
            for &byte in buf {
                let (mut e, s) = ldisc.input_char(byte);
                echo.append(&mut e);
                if s.is_some() {
                    signal = s;
                }
            }
        }

        if !echo.is_empty() {
            let mut master_input = self.input_buffer.lock();
            for &b in &echo {
                if master_input.len() < BUFFER_SIZE {
                    master_input.push_back(b);
                } else {
                    break;
                }
            }
        }

        if let Some(sig) = signal {
            let pgid = self.slave.foreground_pgid.load(Ordering::Acquire);
            if pgid != 0 {
                // SAFETY: the kernel exports this symbol (kernel/src/lib.rs);
                // it delivers to every task in the foreground process group.
                unsafe { tty_signal(pgid, sig as u32) };
            }
        }

        // Release any task blocked reading the slave, plus anything waiting on
        // the master's echo stream. In canonical mode the data may not be
        // readable yet (a partial line), but the parked task re-checks and
        // parks again, so a spurious wakeup costs one loop.
        self.slave.read_waiters.wake_all();
        self.read_waiters.wake_all();

        buf.len()
    }
    
    /// Check if data is available to read
    pub fn has_data(&self) -> bool {
        !self.input_buffer.lock().is_empty()
    }
    
    /// Enable/disable packet mode
    pub fn set_packet_mode(&self, enable: bool) {
        self.packet_mode.store(enable, Ordering::Release);
    }
    
    /// Close master side
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.slave.closed.store(true, Ordering::Release);
        
        // A blocked reader must not stay parked on a terminal that will never
        // produce another byte: end-of-file is the only way out.
        self.slave.read_waiters.wake_all();
        self.read_waiters.wake_all();
        
        *self.slave.master.lock() = None;
    }
}

impl PtySlave {
    /// Get PTY number
    pub fn id(&self) -> u32 {
        self.id
    }
    
    /// Get slave name (e.g., "pts/0")
    pub fn name(&self) -> String {
        alloc::format!("pts/{}", self.id)
    }
    
    /// Read from slave (data from master, post line discipline)
    pub fn read(&self, buf: &mut [u8]) -> usize {
        if self.closed.load(Ordering::Acquire) {
            return 0;
        }
        self.ldisc.lock().read_into(buf)
    }
    
    /// Write to slave (data to master, with OPOST output translation)
    pub fn write(&self, buf: &[u8]) -> usize {
        if self.closed.load(Ordering::Acquire) {
            return 0;
        }
        // XON/XOFF: hold output while stopped, but keep accepting it so the
        // data is not lost — `TCXONC(TCOON)` replays it to the master.
        if self.ldisc.lock().output_stopped() {
            self.ldisc.lock().hold_output(buf);
            return buf.len();
        }
        let out = self.ldisc.lock().process_output(buf);
        
        let master_opt = self.master.lock();
        if let Some(master) = master_opt.as_ref() {
            let mut master_input = master.input_buffer.lock();
            let mut n = 0;
            for &byte in &out {
                if master_input.len() < BUFFER_SIZE {
                    master_input.push_back(byte);
                    n += 1;
                } else {
                    break;
                }
            }
            drop(master_input);
            // A terminal emulator blocked reading the master (waiting to draw
            // what we just wrote) can run now.
            master.read_waiters.wake_all();
            n
        } else {
            0
        }
    }
    
    /// Check if data is available to read (a full line in canonical mode)
    pub fn has_data(&self) -> bool {
        self.ldisc.lock().is_readable()
    }
    
    /// Get termios
    pub fn get_termios(&self) -> Termios {
        self.ldisc.lock().get_termios()
    }
    
    /// Set termios
    pub fn set_termios(&self, termios: Termios) {
        self.ldisc.lock().set_termios(termios);
    }
    
    /// Get window size
    pub fn get_winsize(&self) -> Winsize {
        *self.winsize.lock()
    }
    
    /// Set window size
    pub fn set_winsize(&self, ws: Winsize) {
        *self.winsize.lock() = ws;
    }
    
    /// Get foreground process group
    pub fn get_pgid(&self) -> u32 {
        self.foreground_pgid.load(Ordering::Acquire)
    }
    
    /// Set foreground process group
    pub fn set_pgid(&self, pgid: u32) {
        self.foreground_pgid.store(pgid, Ordering::Release);
    }
    
    /// Ioctl handler for slave. `data` is an ABI-stable byte representation.
    pub fn ioctl(&self, cmd: u32, data: &mut [u8]) -> DriverResult<()> {
        match cmd {
            // The BSD "A" (immediate) variants are accepted as synonyms. On
            // this ABI the flags always take effect immediately anyway, so the
            // distinction POSIX draws between A and S variants does not exist.
            ioctl::TCGETS | ioctl::TCGETA => copy_out(data, &self.get_termios()),
            ioctl::TCSETS | ioctl::TCSETSW | ioctl::TCSETSF
            | ioctl::TCSETA | ioctl::TCSETAW | ioctl::TCSETAF => {
                let t: Termios = copy_in(data)?;
                self.set_termios(t);
                // Changing the discipline can make a parked reader ready (or
                // not): re-evaluate anyone waiting rather than leaving them
                // asleep until the next keystroke.
                self.read_waiters.wake_all();
                Ok(())
            }
            ioctl::TIOCGWINSZ => copy_out(data, &self.get_winsize()),
            ioctl::TIOCSWINSZ => {
                let ws: Winsize = copy_in(data)?;
                self.set_winsize(ws);
                // SIGWINCH is delivered to the foreground group below; a
                // reader parked in a window-size query should re-run.
                self.signal_winsize_change();
                Ok(())
            }
            ioctl::TIOCGPGRP => copy_out(data, &self.get_pgid()),
            ioctl::TIOCSPGRP => {
                let pgid: u32 = copy_in(data)?;
                if pgid == 0 {
                    return Err(DriverError::InvalidArgument);
                }
                // POSIX: only a member of the terminal's session may hand the
                // foreground role to a process group in that same session. The
                // kernel bridge enforces the membership half.
                // SAFETY: the kernel exports this symbol (kernel/src/lib.rs).
                let ok = unsafe { tty_check_pgrp(pgid as u64) };
                if !ok {
                    return Err(DriverError::InvalidArgument);
                }
                self.set_pgid(pgid);
                Ok(())
            }
            ioctl::TIOCGSID => copy_out(data, &self.get_sid()),
            ioctl::FIONREAD => {
                let n: u32 = self.ldisc.lock().available() as u32;
                copy_out(data, &n)
            }
            ioctl::TCXONC => {
                let action: u32 = copy_in(data)?;
                self.tcflow(action)
            }
            ioctl::TCFLSH => {
                let queue: u32 = copy_in(data)?;
                self.tcflush(queue)
            }
            ioctl::TCSBRK => {
                // A real break would need a UART; on a pty there is no line to
                // break, so this is a successful no-op rather than an error.
                let _dur: u32 = copy_in(data)?;
                Ok(())
            }
            _ => Err(DriverError::NotSupported),
        }
    }

    /// Session id owning this terminal, as recorded by the kernel when the
    /// terminal was first opened. Zero when it has no controlling session.
    pub fn get_sid(&self) -> u32 {
        self.session.load(Ordering::Acquire)
    }

    pub fn set_sid(&self, sid: u32) {
        self.session.store(sid, Ordering::Release);
    }

    /// Whether output is currently held by XON/XOFF flow control.
    pub fn output_stopped(&self) -> bool {
        self.ldisc.lock().output_stopped()
    }

    /// `ioctl(TCXONC)`: start, stop, or flush terminal output.
    fn tcflow(&self, action: u32) -> DriverResult<()> {
        // TCOOFF, TCOON, TCIOFF, TCION — Linux values.
        const TCOOFF: u32 = 0;
        const TCOON: u32 = 1;
        const TCIOFF: u32 = 2;
        const TCION: u32 = 3;
        match action {
            TCOOFF => self.ldisc.lock().set_stopped(true),
            TCOON => {
                self.ldisc.lock().set_stopped(false);
                // Replay whatever was written while stopped.
                let pending = core::mem::take(&mut self.ldisc.lock().held_output);
                if !pending.is_empty() {
                    if let Some(master) = self.master.lock().as_ref() {
                        let mut mi = master.input_buffer.lock();
                        for b in pending {
                            if mi.len() < BUFFER_SIZE {
                                mi.push_back(b);
                            }
                        }
                        drop(mi);
                        master.read_waiters.wake_all();
                    }
                }
            }
            TCIOFF => self.ldisc.lock().stop_input(),
            TCION => self.ldisc.lock().resume_input(),
            _ => return Err(DriverError::InvalidArgument),
        }
        self.read_waiters.wake_all();
        Ok(())
    }

    /// `ioctl(TCFLSH)`: discard buffered input, output, or both.
    fn tcflush(&self, queue: u32) -> DriverResult<()> {
        // TCIFLUSH / TCOFLUSH / TCIOFLUSH — Linux values.
        const TCIFLUSH: u32 = 0;
        const TCOFLUSH: u32 = 1;
        match queue {
            TCIFLUSH => self.ldisc.lock().flush_input(),
            TCOFLUSH => self.ldisc.lock().flush_output(),
            _ => {
                self.ldisc.lock().flush_input();
                self.ldisc.lock().flush_output();
            }
        }
        Ok(())
    }

    fn signal_winsize_change(&self) {
        let pgid = self.foreground_pgid.load(Ordering::Acquire);
        if pgid != 0 {
            // SIGWINCH
            // SAFETY: the kernel exports this symbol (kernel/src/lib.rs).
            unsafe { tty_signal(pgid, 28) };
        }
    }
    
    /// Close slave side
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(master) = self.master.lock().as_ref() {
            master.closed.store(true, Ordering::Release);
        }
        self.read_waiters.wake_all();
    }
}

fn copy_in<T: Copy>(data: &[u8]) -> DriverResult<T> {
    if data.len() != core::mem::size_of::<T>() {
        return Err(DriverError::InvalidArgument);
    }
    Ok(unsafe { core::ptr::read_unaligned(data.as_ptr() as *const T) })
}

fn copy_out<T: Copy>(data: &mut [u8], value: &T) -> DriverResult<()> {
    if data.len() != core::mem::size_of::<T>() {
        return Err(DriverError::InvalidArgument);
    }
    unsafe {
        core::ptr::copy_nonoverlapping(
            value as *const T as *const u8,
            data.as_mut_ptr(),
            data.len(),
        );
    }
    Ok(())
}

/// CharDevice implementation for PTY master
pub struct PtyMasterDevice {
    master: Arc<PtyMaster>,
}

impl PtyMasterDevice {
    /// Wrap `master` as a device node. Used by the kernel's `/dev/ptmx` open
    /// path, which allocates the pair itself.
    pub fn new(master: Arc<PtyMaster>) -> Self {
        Self { master }
    }
}

impl CharDevice for PtyMasterDevice {
    fn read(&self, buf: &mut [u8]) -> usize {
        self.master.read(buf)
    }
    
    fn write(&self, buf: &[u8]) -> usize {
        self.master.write(buf)
    }
    
    fn has_data(&self) -> bool {
        self.master.has_data()
    }

    /// `TIOCGPTN` reports which pair this master drives, which is how a caller
    /// that opened `/dev/ptmx` finds the matching `/dev/pts<N>`.
    fn ioctl(&self, cmd: u32, data: &mut [u8]) -> Result<(), ()> {
        match cmd {
            ioctl::TIOCGPTN => {
                let n = self.master.slave.id() as i32;
                if data.len() != core::mem::size_of::<i32>() {
                    return Err(());
                }
                data[..4].copy_from_slice(&n.to_ne_bytes());
                Ok(())
            }
            // Linux requires the slave to be unlocked with `TIOCSPTLCK` before
            // it can be opened. A pty slave here is never locked, so accept
            // the call rather than making callers special-case it.
            ioctl::TIOCSPTLCK => Ok(()),
            _ => Err(()),
        }
    }

    /// Register `task` for a wakeup when the master has something to draw.
    /// Returns `true` when data is already queued, so the caller can skip the
    /// park entirely.
    fn park(&self, task: usize, interest: u16) -> bool {
        // Gated on POLLIN. Claiming "ready" because the master has queued input
        // to a caller that asked only about writability is the livelock
        // described on `Vnode::poll_park`: the poller cancels and re-probes
        // forever. A master is always writable, so a POLLOUT-only wait never
        // reaches here at all -- `poll_events` already reported it ready.
        let want_read = interest & driver_common::POLLIN != 0;
        if want_read {
            if self.master.closed.load(Ordering::Acquire) {
                return true;
            }
            if self.master.has_data() {
                return true;
            }
            // SAFETY: the kernel exports this symbol (kernel/src/lib.rs).
            self.master.read_waiters.register(unsafe { tty_current_task() });
        }
        let _ = task;
        false
    }

    fn unpark(&self, _task: usize) {
        // SAFETY: the kernel exports this symbol (kernel/src/lib.rs).
        self.master.read_waiters.unregister(unsafe { tty_current_task() });
    }
    
    fn name(&self) -> &'static str {
        "pty-master"
    }
}

/// CharDevice implementation for PTY slave
pub struct PtySlaveDevice {
    slave: Arc<PtySlave>,
}

impl PtySlaveDevice {
    /// Wrap `slave` as a device node. Used by the kernel's `/dev/ptmx` open
    /// path, which publishes the slave of a freshly allocated pair.
    pub fn new(slave: Arc<PtySlave>) -> Self {
        Self { slave }
    }
}

impl CharDevice for PtySlaveDevice {
    fn read(&self, buf: &mut [u8]) -> usize {
        self.slave.read(buf)
    }
    
    fn write(&self, buf: &[u8]) -> usize {
        self.slave.write(buf)
    }
    
    fn has_data(&self) -> bool {
        self.slave.has_data()
    }

    fn ioctl(&self, cmd: u32, data: &mut [u8]) -> Result<(), ()> {
        self.slave.ioctl(cmd, data).map_err(|_| ())
    }

    /// This is a terminal: termios, echoing, and `ISIG` delivery. The VFS uses
    /// that to make a blocking `read` wait for input instead of reporting
    /// end-of-file, which is what a libc's stdio requires.
    fn is_terminal(&self) -> bool {
        true
    }

    /// Claim this terminal for session `sid` if nothing owns it yet. The
    /// foreground process group follows the owning session, so a shell that
    /// opens a terminal becomes the foreground group by default.
    fn acquire_session(&self, sid: u32) {
        if sid == 0 {
            return;
        }
        if self.slave.session.compare_exchange(
            0,
            sid,
            Ordering::AcqRel,
            Ordering::Acquire,
        ).is_ok()
        {
            // Nothing was foreground before; the owning session leads it.
            self.slave
                .foreground_pgid
                .compare_exchange(0, sid, Ordering::AcqRel, Ordering::Acquire)
                .ok();
        }
    }

    /// Register `task` for a wakeup when input (or an end-of-file from `^D`)
    /// is available on the slave. Returns `true` when the terminal is already
    /// readable, so the caller proceeds without parking.
    fn park(&self, task: usize, interest: u16) -> bool {
        // Gated on POLLIN, for the same reason as the master above: readiness
        // has to be reported for the direction the caller asked about.
        let want_read = interest & driver_common::POLLIN != 0;
        if want_read {
            if self.slave.closed.load(Ordering::Acquire) {
                // Closed terminal: report ready so the reader observes EOF.
                return true;
            }
            if self.slave.has_data() {
                return true;
            }
            // SAFETY: the kernel exports this symbol (kernel/src/lib.rs).
            self.slave.read_waiters.register(unsafe { tty_current_task() });
        }
        let _ = task;
        false
    }

    fn unpark(&self, _task: usize) {
        // SAFETY: the kernel exports this symbol (kernel/src/lib.rs).
        self.slave.read_waiters.unregister(unsafe { tty_current_task() });
    }
    
    fn name(&self) -> &'static str {
        "pty-slave"
    }
}

/// The `/dev/ptmx` multiplexer.
///
/// Linux semantics: `/dev/ptmx` is not a terminal itself. Opening it allocates
/// a *new* master/slave pair, and the slave of that pair appears as
/// `/dev/pts<N>`. A program that hard-codes a single pair — or that shares one
/// with another program — gets its input stolen by whichever reader wins the
/// race, so allocation has to happen per open.
///
/// Samsara previously bound `/dev/ptmx` to one fixed pair at init, which made
/// every user of the multiplexer share a single terminal. This device fixes
/// that: pair 0 stays reserved for the boot-time console front end (which
/// refers to `/dev/pts0` by name), and each open of `/dev/ptmx` hands out a
/// fresh pair from 1 upward.
pub struct PtmxDevice;

impl CharDevice for PtmxDevice {
    fn read(&self, _buf: &mut [u8]) -> usize {
        0
    }

    fn write(&self, _buf: &[u8]) -> usize {
        0
    }

    fn has_data(&self) -> bool {
        false
    }

    fn name(&self) -> &'static str {
        "ptmx"
    }
}

/// Allocate a fresh master/slave pair for a multiplexer open, returning the
/// master and the pair index the slave will be published under.
///
/// The caller (the kernel's `open` path) publishes `/dev/pts<index>` and
/// installs `master` as the descriptor's node, which is what makes one
/// `open("/dev/ptmx")` yield an independent terminal.
pub fn open_ptmx() -> Option<(Arc<PtyMaster>, u32)> {
    let (master, slave) = allocate().ok()?;
    Some((master, slave.id()))
}

/// The slave of a master, so the kernel can publish it in devfs under the
/// `/dev/pts<N>` name that matches [`open_ptmx`]'s index.
pub fn slave_of(master: &Arc<PtyMaster>) -> Arc<PtySlave> {
    master.slave.clone()
}

/// Register the boot-time pty pair plus the `/dev/ptmx` multiplexer.
pub fn register_devices() -> DriverResult<()> {
    // Pair 0 is the boot console's terminal, published under a fixed name
    // because the installer and terminal refer to `/dev/pts0` directly.
    let (_master0, slave0) = allocate()?;
    register_char_device("pts0", Arc::new(PtySlaveDevice { slave: slave0.clone() }))?;
    // The master of pair 0 is reachable as `/dev/ptmx0` for anything that
    // wants to drive the console terminal directly; `/dev/ptmx` itself is the
    // allocating multiplexer.
    let master0 = slave0.master.lock().clone();
    if let Some(m) = master0 {
        register_char_device("ptmx0", Arc::new(PtyMasterDevice { master: m }))?;
    }
    register_char_device("ptmx", Arc::new(PtmxDevice))?;
    driver_common::kinfo!("pty: registered ptmx (multiplexer) and pts0 (boot console)");
    Ok(())
}

/// PTY driver entry point
pub struct PtyDriver;

impl DriverEntry for PtyDriver {
    fn probe(&mut self) -> DriverResult<Vec<DeviceInfo>> {
        Ok(vec![DeviceInfo {
            id: driver_common::DeviceId::new(0, 0, 0),
            class: DeviceClass::Char,
            vendor_id: 0,
            device_id: 0,
            revision: 0,
            capabilities: DeviceCapabilities::READ | DeviceCapabilities::WRITE | DeviceCapabilities::IOCTL,
            name: alloc::string::String::from("PTY"),
            driver_name: Some(alloc::string::String::from("pty")),
        }])
    }
    
    fn attach(&mut self, _device: DeviceInfo) -> DriverResult<()> {
        register_devices()
    }
    
    fn detach(&mut self, _device_id: driver_common::DeviceId) -> DriverResult<()> {
        Ok(())
    }
}

/// PTY driver capabilities (const-evaluable via `from_bits_truncate`).
const PTY_CAPABILITIES: DeviceCapabilities = DeviceCapabilities::from_bits_truncate(
    DeviceCapabilities::READ.bits()
        | DeviceCapabilities::WRITE.bits()
        | DeviceCapabilities::IOCTL.bits(),
);

pub static PTY_DRIVER: DriverRegistration = DriverRegistration {
    name: "pty",
    version: "1.0.0",
    classes: &[DeviceClass::Char],
    capabilities: PTY_CAPABILITIES,
};
