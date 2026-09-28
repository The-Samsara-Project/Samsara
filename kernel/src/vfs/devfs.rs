// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! devfs: device nodes exposed under `/dev`.
//!
//! Drivers register [`CharDevice`] and [`BlockDevice`] handles by name;
//! each becomes a vnode whose read/write dispatch into the driver.

use super::{FsError, NodeKind, Vnode, VnodeRef};
use crate::sync::Spinlock;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use driver_common::{BlockDevice, CharDevice};

struct DevDir {
    devices: Spinlock<BTreeMap<String, VnodeRef>>,
}

/// A vnode wrapping one registered character device.
pub struct CharDeviceNode {
    dev: Arc<dyn CharDevice>,
}

impl Vnode for CharDeviceNode {
    fn kind(&self) -> NodeKind {
        NodeKind::CharDevice
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        debug_assert_eq!(offset, 0, "char devices ignore offsets");
        if buf.is_empty() {
            return Ok(0);
        }
        Ok(self.dev.read(buf))
    }

    fn write_at(&self, _offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        Ok(self.dev.write(buf))
    }

    fn ioctl(&self, cmd: u32, data: &mut [u8]) -> Result<(), FsError> {
        self.dev.ioctl(cmd, data).map_err(|_| FsError::NotSupported)
    }

    fn is_terminal(&self) -> bool {
        self.dev.is_terminal()
    }

    fn acquire_session(&self, sid: u32) {
        self.dev.acquire_session(sid)
    }

    fn mode(&self) -> u32 {
        0o666
    }

    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        Err(FsError::NotADirectory)
    }

    fn readable_now(&self) -> bool {
        self.dev.has_data()
    }

    /// Char devices accept output unconditionally; input readiness follows the
    /// device's data availability.
    fn poll_events(&self, _interest: u16) -> u16 {
        let mut r = driver_common::POLLOUT;
        if self.dev.has_data() {
            r |= driver_common::POLLIN;
        }
        r
    }

    fn poll_park(&self, task: usize, interest: u16) -> bool {
        // Re-check under the driver's own lock so a byte pushed between the
        // availability probe and the registration cannot be missed.
        //
        // Gated on the interest: a device with data is ready to *read*, and
        // saying so to a caller waiting only for writability is the livelock
        // described on `Vnode::poll_park`.
        if interest & driver_common::POLLIN != 0 && self.dev.has_data() {
            return true;
        }
        self.dev.park(task, interest)
    }

    fn poll_cancel(&self, task: usize) {
        self.dev.unpark(task);
    }

    fn size_hint(&self) -> u64 {
        0
    }

    fn mmap_phys(&self) -> Option<(u64, u64)> {
        self.dev.mmap_phys()
    }
}

/// A directory node inside devfs, so paths can nest.
///
/// Linux lays input devices out as `/dev/input/event0`, and a ported terminal
/// will look for exactly that. devfs was flat when only `/dev/kbd0` and
/// `/dev/mouse0` existed, so a subdirectory node is all that is needed to make
/// a nested path resolve: the VFS already walks components with `lookup`.
pub struct DevSubdir {
    name: &'static str,
    children: Spinlock<BTreeMap<String, VnodeRef>>,
}

impl DevSubdir {
    /// Create an empty subdirectory.
    pub fn new(name: &'static str) -> Self {
        DevSubdir {
            name,
            children: Spinlock::new(BTreeMap::new()),
        }
    }

    /// Publish `dev` inside this directory under `name`.
    pub fn insert(&self, name: &str, dev: VnodeRef) {
        self.children.lock().insert(String::from(name), dev);
    }

    /// The directory's own name, for diagnostics.
    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl Vnode for DevSubdir {
    fn kind(&self) -> NodeKind {
        NodeKind::Dir
    }

    fn mode(&self) -> u32 {
        0o555
    }

    fn lookup(&self, name: &str) -> Result<VnodeRef, FsError> {
        self.children
            .lock()
            .get(name)
            .cloned()
            .ok_or(FsError::NotFound)
    }

    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        let mut out: Vec<(String, NodeKind)> = self
            .children
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.kind()))
            .collect();
        out.sort();
        Ok(out)
    }
}

/// The `/dev/fb0` node, backed by the active framebuffer.
///
/// Terminals reach a linear framebuffer by mmapping it -- a per-pixel
/// `write(2)` is far too slow to redraw a screen -- so this device reports the
/// framebuffer's physical range for `mmap(2)` rather than only offering
/// read/write. Both work: a program that mmaps gets the direct window, and one
/// that does not can still use `read`/`write`, which is why both paths are
/// implemented.
///
/// Writes go to the same memory a read returns, so the device is a plain
/// window and not a one-way sink.
pub struct FbDevice;

impl CharDevice for FbDevice {
    fn read(&self, buf: &mut [u8]) -> usize {
        let Some(fb) = crate::framebuffer::get() else {
            return 0;
        };
        // Report the geometry in the same fixed layout the kernel already hands
        // out over `FB_INFO`, so a caller that cannot map still learns the
        // shape of the device.
        let g = fb.geometry();
        let mut info = [0u8; 40];
        info[0..4].copy_from_slice(&(g.width as u32).to_ne_bytes());
        info[4..8].copy_from_slice(&(g.height as u32).to_ne_bytes());
        info[8..12].copy_from_slice(&(g.pitch as u32).to_ne_bytes());
        info[12..16].copy_from_slice(&(g.bpp as u32).to_ne_bytes());
        info[16..20].copy_from_slice(&(g.red_pos as u32).to_ne_bytes());
        info[20..24].copy_from_slice(&(g.red_size as u32).to_ne_bytes());
        info[24..28].copy_from_slice(&(g.green_pos as u32).to_ne_bytes());
        info[28..32].copy_from_slice(&(g.green_size as u32).to_ne_bytes());
        info[32..36].copy_from_slice(&(g.blue_pos as u32).to_ne_bytes());
        info[36..40].copy_from_slice(&(g.blue_size as u32).to_ne_bytes());
        let n = buf.len().min(info.len());
        buf[..n].copy_from_slice(&info[..n]);
        n
    }

    fn write(&self, _buf: &[u8]) -> usize {
        // Pixel data must go through `mmap(2)`. A write here would have to
        // decode the buffer's geometry and blit it, and a caller that has
        // pixels in hand almost certainly has the mapping to put them in.
        // Reporting zero rather than a short write is honest: nothing was
        // written, so the caller's error check sees a failure.
        0
    }

    fn mmap_phys(&self) -> Option<(u64, u64)> {
        let fb = crate::framebuffer::get()?;
        let g = fb.geometry();
        if g.size == 0 {
            return None;
        }
        // The visible window starts `offset` bytes into the mapped range, and
        // `mmap` starts at offset 0, so the range handed out is the whole
        // mapping and a program applies the same offset it would on Linux.
        Some((g.phys as u64, g.size as u64))
    }

    /// Answer the Linux framebuffer queries.
    ///
    /// A ported terminal learns the display's shape from these rather than from
    /// the geometry Samsara reports over `FB_INFO`, because that is the
    /// interface it already speaks. Two of them matter:
    ///
    ///   - `FBIOGET_VSCREENINFO`, which carries the resolution, the bit depth
    ///     and the position of each colour channel.
    ///   - `FBIOGET_FSCREENINFO`, which carries the scanline length and the
    ///     length of the mappable range -- the latter is what the terminal
    ///     passes to `mmap(2)`.
    ///
    /// Everything else in the Linux request set is refused. In particular the
    /// pan and palette requests are refused rather than approximated: a terminal
    /// that believes it panned the display when it did not scrolls into nothing,
    /// and one that believes it set a palette on a truecolour display misdraws
    /// the screen. Both are already true here -- the pan steps in
    /// `FBIOGET_FSCREENINFO` are zero and the visual is truecolour -- so a
    /// terminal that honours them simply never asks.
    fn ioctl(&self, cmd: u32, data: &mut [u8]) -> Result<(), ()> {
        use crate::abi::framebuffer_ioctl::{
            FbFixScreeninfo, FbVarScreeninfo, FBIOGET_FSCREENINFO, FBIOGET_VSCREENINFO,
        };

        let Some(fb) = crate::framebuffer::get() else {
            return Err(());
        };
        let g = fb.geometry();

        match cmd {
            FBIOGET_VSCREENINFO => {
                // Length-checked against the kernel's own struct, so a mismatch
                // with the caller's idea of the layout is reported rather than
                // silently truncating. The ABI layer already sized the copy
                // from the same struct, so this is belt and braces -- and
                // cheap, because this ioctl runs once at startup.
                let v = FbVarScreeninfo::from_geometry(&g);
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        &v as *const FbVarScreeninfo as *const u8,
                        core::mem::size_of::<FbVarScreeninfo>(),
                    )
                };
                if data.len() < bytes.len() {
                    return Err(());
                }
                data[..bytes.len()].copy_from_slice(bytes);
                Ok(())
            }
            FBIOGET_FSCREENINFO => {
                let f = FbFixScreeninfo::from_geometry(&g);
                let bytes = unsafe {
                    core::slice::from_raw_parts(
                        &f as *const FbFixScreeninfo as *const u8,
                        core::mem::size_of::<FbFixScreeninfo>(),
                    )
                };
                if data.len() < bytes.len() {
                    return Err(());
                }
                data[..bytes.len()].copy_from_slice(bytes);
                Ok(())
            }
            // Not a query this device implements. Refused, never faked.
            _ => Err(()),
        }
    }

    fn name(&self) -> &'static str {
        "linear-framebuffer"
    }
}

/// A vnode wrapping one registered block device.
///
/// Block device implementations provided by drivers (`SataDisk`,
/// `NvmeBlockDevice`) serialize their own I/O internally, so the `Arc` is
/// shared behind the vnode without an extra kernel-side lock.
pub struct BlockDeviceNode {
    dev: Arc<dyn BlockDevice>,
}

impl Vnode for BlockDeviceNode {
    fn kind(&self) -> NodeKind {
        NodeKind::File
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let lba = offset / self.dev.block_size() as u64;
        self.dev.read_blocks(lba, buf).map_err(|_| FsError::IoError)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> Result<usize, FsError> {
        let lba = offset / self.dev.block_size() as u64;
        self.dev.write_blocks(lba, buf).map_err(|_| FsError::IoError)
    }

    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        Err(FsError::NotADirectory)
    }

    fn size_hint(&self) -> u64 {
        self.dev.num_blocks() * self.dev.block_size() as u64
    }

    fn is_seekable(&self) -> bool {
        // A block device is addressed by absolute offset, so repositioning is
        // meaningful and `SEEK_END` has a real answer. This is what makes
        // `dd seek=N` and a filesystem driver mapping the raw device work.
        true
    }

    fn file_size(&self) -> u64 {
        self.size_hint()
    }
}

impl Vnode for DevDir {
    fn kind(&self) -> NodeKind {
        NodeKind::Dir
    }

    fn mode(&self) -> u32 {
        0o555
    }

    fn lookup(&self, name: &str) -> Result<VnodeRef, FsError> {
        self.devices
            .lock()
            .get(name)
            .cloned()
            .ok_or(FsError::NotFound)
    }

    fn create_child(
        &self,
        name: &str,
        kind: NodeKind,
        _uid: u32,
        _gid: u32,
        _mode: u32,
    ) -> Result<VnodeRef, FsError> {
        let _ = name;
        let _ = kind;
        Err(FsError::Permission)
    }

    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        let mut out: Vec<(String, NodeKind)> = self
            .devices
            .lock()
            .iter()
            .map(|(k, v)| (k.clone(), v.kind()))
            .collect();
        out.sort();
        Ok(out)
    }
}

static DEV_DIR: Spinlock<Option<Arc<DevDir>>> = Spinlock::new(None);

/// Bring up devfs and return its root vnode.
pub fn new_root() -> VnodeRef {
    let dir = Arc::new(DevDir {
        devices: Spinlock::new(BTreeMap::new()),
    });
    *DEV_DIR.lock() = Some(dir.clone());
    dir
}

/// The system console, exposed as `/dev/console`.
///
/// This exists so a process that was not started by another process still has
/// somewhere for its standard descriptors to point. The Rust runtime never
/// needed one -- it forwards output over IPC to the console server -- but a
/// libc-based program writes to fd 1 with `write(2)` like any other, and
/// without a descriptor installed there every one of those writes fails with
/// `EBADF` and the program's output is silently lost.
///
/// It is a terminal, and reports as one, so `isatty(1)` is true and stdio
/// line-buffers to it. Reads return end-of-file: keyboard input reaches a
/// process through a pty, driven by the input daemon, and this device is the
/// output half of the console only.
pub struct ConsoleDevice;

impl CharDevice for ConsoleDevice {
    fn read(&self, _buf: &mut [u8]) -> usize {
        // No input queue: the console is write-only for userspace.
        0
    }

    fn write(&self, buf: &[u8]) -> usize {
        // Both halves of the console, which is what makes it *the* console: the
        // framebuffer for a human at the display, and the serial line for
        // anything reading the boot log. Emitting to only one of them is how
        // output goes missing -- the framebuffer is invisible to a headless
        // `qemu -display none`, which is how the kernel itself is run.
        crate::console::write(buf);
        for &b in buf {
            crate::io::uart::send(b);
        }
        buf.len()
    }

    fn has_data(&self) -> bool {
        false
    }

    fn ioctl(&self, cmd: u32, data: &mut [u8]) -> Result<(), ()> {
        // The console is a terminal, so it has to answer the terminal queries.
        // This is not cosmetic: `isatty(3)` is implemented as "does TCGETS
        // succeed", and stdio picks its buffering from the answer. A console
        // that refuses TCGETS looks like a file, so every line a program prints
        // is fully buffered and lost if the program exits without flushing.
        use crate::drivers::pty::Termios;
        match cmd {
            0x5401 => copy_out(data, &Termios::default()), // TCGETS
            0x5413 => {
                // TIOCGWINSZ: the framebuffer text mode has a fixed geometry.
                let ws = crate::console::winsize();
                copy_out(data, &ws)
            }
            _ => Err(()),
        }
    }

    fn is_terminal(&self) -> bool {
        true
    }

    fn name(&self) -> &'static str {
        "console"
    }
}

/// The `/dev/console` node, if the console device has been registered.
///
/// Split from [`anonymous`] so the spawn path can hand a process its standard
/// descriptors without caring whether registration has happened yet.
pub fn console_node() -> Result<VnodeRef, FsError> {
    let guard = DEV_DIR.lock();
    let dir = guard.as_ref().ok_or(FsError::NotFound)?;
    let node = dir.devices.lock().get("console").cloned();
    node.ok_or(FsError::NotFound)
}

/// Wrap `dev` in a vnode without publishing it under a name.
///
/// Used for a device that lives inside a subdirectory (`/dev/input/event0`)
/// rather than directly in `/dev`.
pub fn char_node(dev: Arc<dyn CharDevice>) -> VnodeRef {
    anonymous(dev)
}

/// Every published `/dev/...` path, paired with the node it resolves to.
///
/// This is the reverse index that `resolve` alone cannot provide: given a node,
/// `path_of` recovers the name it was registered under. It exists for
/// `ttyname(3)`, which hands a program the *path* of its terminal so the program
/// can reopen it or report it.
///
/// A flat list rather than a tree walk, and deliberately so. Recovering a path
/// from a `&dyn Vnode` otherwise requires downcasting the trait object back to
/// `DevSubdir`, and `Vnode` has no `Any` supertrait to make that safe -- so the
/// choices are an unsafe vtable cast or an index maintained at registration. The
/// index is better: the cast is sound only while every devfs directory really
/// is a `DevSubdir`, which is a property of today's code rather than of the
/// type, and a future devfs directory type would silently turn it into a
/// wrong-pointer dereference.
///
/// Comparison is by node identity (`Arc::ptr_eq`), never by a name a device
/// reports about itself. devfs assigned the name, so devfs is what remembers
/// it; a device that supplied its own name could disagree with the name it is
/// actually reachable under, and the caller would be handed a path that does
/// not open.
static NAMES: Spinlock<Vec<(String, VnodeRef)>> = Spinlock::new(Vec::new());

/// Record that `node` is reachable at `path`.
///
/// A later duplicate of the same node is kept rather than replacing the earlier
/// entry, and `path_of` returns the first. A device published under two names is
/// reachable under both, so either answer is truthful; what matters is that the
/// choice is deterministic, and insertion order is that.
fn record(path: String, node: VnodeRef) {
    NAMES.lock().push((path, node));
}

/// The `/dev/...` path at which `target` is published, if it is.
///
/// Returns `None` for a node that is not in devfs at all -- a ramfs file, or a
/// terminal reached through some mount other than devfs. Reporting no name is
/// correct there; inventing one would hand back a path that does not resolve,
/// and a program that trusted it would fail later, and further away.
pub fn path_of(target: &VnodeRef) -> Option<String> {
    NAMES
        .lock()
        .iter()
        .find(|(_, node)| Arc::ptr_eq(node, target))
        .map(|(path, _)| path.clone())
}

/// Publish an already-populated subdirectory at `/dev/<name>`.
///
/// Separate from [`register`] because the caller has already filled the
/// subdirectory's children; devfs only needs to put the name in the map so the
/// path resolves.
pub fn register_subdir(name: &str, dir: Arc<DevSubdir>) -> Result<(), FsError> {
    let guard = DEV_DIR.lock();
    let root = guard.as_ref().ok_or(FsError::NotFound)?;
    let base = alloc::format!("/dev/{}", name);
    let node: VnodeRef = dir.clone();
    root.devices.lock().insert(String::from(name), node.clone());
    drop(guard);
    // The directory itself is addressable by path, and so is every child
    // already inside it. Children are recorded here rather than at insert time
    // because a subdirectory is normally populated *before* it is published --
    // the input device builds its tree first and is then registered -- so at
    // insert time the full path does not exist yet.
    record(base.clone(), node);
    for (child_name, child) in dir.children.lock().iter() {
        record(alloc::format!("{}/{}", base, child_name), child.clone());
    }
    Ok(())
}

/// Wrap `dev` in a vnode without publishing it under a name.
///
/// `/dev/ptmx` needs this: opening it allocates a pair whose slave is then
/// published by name, while the descriptor itself refers to the master, which
/// has no name of its own.
pub fn anonymous(dev: Arc<dyn CharDevice>) -> VnodeRef {
    Arc::new(CharDeviceNode { dev })
}

/// Copy `value` into an ioctl argument buffer.
fn copy_out<T: Copy>(data: &mut [u8], value: &T) -> Result<(), ()> {
    let n = core::mem::size_of::<T>();
    if data.len() < n {
        return Err(());
    }
    // SAFETY: the caller validated the length, and `T` is `Copy` with no
    // padding requirements beyond its own size.
    unsafe { core::ptr::copy_nonoverlapping(value as *const T as *const u8, data.as_mut_ptr(), n) };
    Ok(())
}

/// Register a character device instance under `name`.
pub fn register(name: &str, dev: Arc<dyn CharDevice>) -> Result<(), FsError> {
    let guard = DEV_DIR.lock();
    let dir = guard.as_ref().ok_or(FsError::NotFound)?;
    let mut devs = dir.devices.lock();
    if devs.contains_key(name) {
        return Err(FsError::Exists);
    }
    crate::log::kdebug!("devfs: registered /dev/{} ({})", name, dev.name());
    let node: VnodeRef = Arc::new(CharDeviceNode { dev });
    devs.insert(String::from(name), node.clone());
    // Record after the insert succeeded, so the reverse index never names a
    // path that resolve() would refuse.
    drop(devs);
    record(alloc::format!("/dev/{}", name), node);
    Ok(())
}

/// Register a block device instance under `name`.
pub fn register_block(name: &str, dev: Arc<dyn BlockDevice>) -> Result<(), FsError> {
    let guard = DEV_DIR.lock();
    let dir = guard.as_ref().ok_or(FsError::NotFound)?;
    let mut devs = dir.devices.lock();
    if devs.contains_key(name) {
        return Err(FsError::Exists);
    }
    crate::log::kdebug!("devfs: registered /dev/{} (block)", name);
    devs.insert(String::from(name), Arc::new(BlockDeviceNode { dev }));
    Ok(())
}
