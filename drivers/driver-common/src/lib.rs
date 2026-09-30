// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Common infrastructure for Samsara device drivers.

#![no_std]
#![deny(missing_docs)]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use bitflags::bitflags;

/// Result type for driver operations.
pub type DriverResult<T> = Result<T, DriverError>;

/// Errors that can occur during driver operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DriverError {
    /// Operation not supported by the driver
    NotSupported = 1,
    /// Resource not found
    NotFound = 2,
    /// Invalid argument provided
    InvalidArgument = 3,
    /// I/O error occurred
    IoError = 4,
    /// Device is busy
    Busy = 5,
    /// Out of memory
    OutOfMemory = 6,
    /// Device not ready
    NotReady = 7,
    /// Permission denied
    PermissionDenied = 8,
    /// Invalid state for operation
    InvalidState = 9,
    /// Operation timed out
    Timeout = 10,
    /// Interrupt not handled
    InterruptNotHandled = 11,
    /// DMA error
    DmaError = 12,
    /// Invalid DMA address
    InvalidDmaAddress = 13,
    /// Buffer too small
    BufferTooSmall = 14,
    /// Alignment error
    AlignmentError = 15,
    /// Resource already claimed
    AlreadyClaimed = 16,
    /// Resource not claimed
    NotClaimed = 17,
    /// Hardware fault
    HardwareFault = 18,
    /// Feature not supported
    UnsupportedFeature = 19,
    /// Operation cancelled
    Cancelled = 20,
}

/// Device classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum DeviceClass {
    /// Unknown device class
    Unknown = 0,
    /// Block device (disk, etc.)
    Block = 1,
    /// Character device (serial, etc.)
    Char = 2,
    /// Network device
    Network = 3,
    /// Display device
    Display = 4,
    /// Input device (keyboard, mouse)
    Input = 5,
    /// Audio device
    Audio = 6,
    /// USB device
    Usb = 7,
    /// PCI device
    Pci = 8,
    /// NVMe device
    Nvme = 9,
    /// AHCI device
    Ahci = 10,
    /// Serial device
    Serial = 11,
    /// GPIO device
    Gpio = 12,
    /// Interrupt controller
    InterruptController = 13,
    /// Timer device
    Timer = 14,
    /// RTC device
    Rtc = 15,
    /// Power management device
    Power = 16,
    /// Sensor device
    Sensor = 17,
    /// Virtual device
    Virtual = 18,
}

/// Device capabilities bitflags
///
/// Provides a set of capability flags for device drivers.
bitflags! {
    /// Device capability flags
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct DeviceCapabilities: u64 {
        /// Read capability
        const READ = 1 << 0;
        /// Write capability
        const WRITE = 1 << 1;
        /// Seek capability
        const SEEK = 1 << 2;
        /// Memory map capability
        const MAP = 1 << 3;
        /// IOCTL capability
        const IOCTL = 1 << 4;
        /// Poll capability
        const POLL = 1 << 29;
        /// Mmap capability
        const MMAP = 1 << 5;
        /// Lock capability
        const LOCK = 1 << 6;
        /// Exclusive access capability
        const EXCLUSIVE = 1 << 7;
        /// Blocking I/O capability
        const BLOCKING = 1 << 8;
        /// Non-blocking I/O capability
        const NONBLOCKING = 1 << 9;
        /// Async I/O capability
        const ASYNC = 1 << 10;
        /// DMA capability
        const DMA = 1 << 11;
        /// PIO capability
        const PIO = 1 << 12;
        /// MMIO capability
        const MMIO = 1 << 13;
        /// Interrupt capability
        const INTERRUPT = 1 << 14;
        /// MSI capability
        const MSI = 1 << 15;
        /// MSI-X capability
        const MSI_X = 1 << 16;
        /// Hotplug capability
        const HOTPLUG = 1 << 17;
        /// Power management capability
        const POWER_MANAGEMENT = 1 << 18;
        /// Hot unplug capability
        const HOT_UNPLUG = 1 << 19;
        /// Partitions capability
        const PARTITIONS = 1 << 20;
        /// Encryption capability
        const ENCRYPTION = 1 << 21;
        /// Compression capability
        const COMPRESSION = 1 << 22;
        /// TRIM capability
        const TRIM = 1 << 23;
        /// Flush capability
        const FLUSH = 1 << 24;
        /// Barrier capability
        const BARRIER = 1 << 25;
        /// FUA capability
        const FUA = 1 << 26;
        /// Zoned storage capability
        const ZONED = 1 << 27;
        /// Namespace capability
        const NAMESPACE = 1 << 28;
    }
}

/// PCI device identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId {
    /// Bus number
    pub bus: u32,
    /// Device number
    pub device: u32,
    /// Function number
    pub function: u8,
}

impl DeviceId {
    /// Create a new device ID
    pub const fn new(bus: u32, device: u32, function: u8) -> Self {
        Self { bus, device, function }
    }
}

/// Device information
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    /// Device identifier
    pub id: DeviceId,
    /// Device class
    pub class: DeviceClass,
    /// PCI vendor ID
    pub vendor_id: u16,
    /// PCI device ID
    pub device_id: u16,
    /// Device revision
    pub revision: u8,
    /// Device capabilities
    pub capabilities: DeviceCapabilities,
    /// Device name
    pub name: String,
    /// Driver name
    pub driver_name: Option<String>,
}

/// Driver registration info
pub struct DriverRegistration {
    /// Driver name
    pub name: &'static str,
    /// Driver version
    pub version: &'static str,
    /// Supported device classes
    pub classes: &'static [DeviceClass],
    /// Driver capabilities
    pub capabilities: DeviceCapabilities,
}

/// Driver entry point trait
pub trait DriverEntry: Send + Sync {
    /// Probe for devices
    fn probe(&mut self) -> DriverResult<Vec<DeviceInfo>>;
    /// Attach to a device
    fn attach(&mut self, device: DeviceInfo) -> DriverResult<()>;
    /// Detach from a device
    fn detach(&mut self, device_id: DeviceId) -> DriverResult<()>;
}

/// DMA memory allocator interface - implemented by the kernel
extern "Rust" {
    /// Allocate physically contiguous DMA pages
    pub fn alloc_dma_pages(pages: usize) -> Option<u64>;

    /// Free DMA pages
    pub fn free_dma_pages(phys: u64, pages: usize);

    /// Convert physical address to kernel virtual address
    pub fn phys_to_virt(phys: u64) -> u64;

    /// Convert kernel virtual address to physical address
    pub fn virt_to_phys(virt: u64) -> u64;
}

/// Block device trait for storage drivers
pub trait BlockDevice: Send + Sync {
    /// Read blocks from the device
    fn read_blocks(&self, lba: u64, blocks: &mut [u8]) -> DriverResult<usize>;

    /// Write blocks to the device
    fn write_blocks(&self, lba: u64, blocks: &[u8]) -> DriverResult<usize>;

    /// Flush cached data to media
    fn flush(&self) -> DriverResult<()>;

    /// Get block size in bytes
    fn block_size(&self) -> u32;

    /// Get total number of blocks
    fn num_blocks(&self) -> u64;

    /// Get device info
    fn device_info(&self) -> DeviceInfo;
}

/// Character device trait
pub trait CharDevice: Send + Sync {
    /// Read from device
    fn read(&self, buf: &mut [u8]) -> usize;
    /// Write to device
    fn write(&self, buf: &[u8]) -> usize;
    /// Check if data is available
    fn has_data(&self) -> bool {
        false
    }
    /// Register `task` for a wakeup when data arrives, checked-and-registered
    /// under the device's own lock. Returns `true` if data is already available
    /// (no registration needed); `false` once `task` is queued. No-op for
    /// policies that cannot block (an output-only device). Only needed when
    /// `has_data` can be false.
    ///
    /// `interest` is the same `POLL_*` mask the caller is waiting on, and a
    /// device must honour it for the same reason [`Vnode::poll_park`] does:
    /// reporting readiness for a direction nobody asked about turns a bounded
    /// wait into a spin.
    fn park(&self, _task: usize, _interest: u16) -> bool {
        false
    }
    /// Undo a [`park`] registration; idempotent.
    fn unpark(&self, _task: usize) {}
    /// Perform a device-specific control request on an in-kernel byte buffer.
    /// The VFS validates and copies the caller's user buffer before dispatch.
    fn ioctl(&self, _cmd: u32, _data: &mut [u8]) -> Result<(), ()> {
        Err(())
    }
    /// Whether this device is a terminal. See [`Vnode::is_terminal`] for why
    /// the answer matters; a device that does not speak termios leaves this
    /// false.
    fn is_terminal(&self) -> bool {
        false
    }
    /// Record `sid` as the session controlling this terminal, but only if no
    /// session has claimed it yet.
    ///
    /// This is the controlling-terminal assignment POSIX performs when a
    /// process opens a terminal it does not already have: the first opener
    /// becomes the owner and `TIOCGSID` reports that session. A terminal that
    /// already has an owner keeps it, so opening the same terminal twice does
    /// not silently transfer it.
    fn acquire_session(&self, _sid: u32) {}
    /// Physical range this device occupies, for `mmap(2)`.
    ///
    /// `Some((phys, len))` lets a process map the device directly instead of
    /// copying every byte through `read`/`write`. The default is `None`, which
    /// is correct for every device whose traffic is a byte stream, and is the
    /// reason a `write(2)`-based fallback to `read`/`write` remains available
    /// for terminals that cannot map.
    fn mmap_phys(&self) -> Option<(u64, u64)> {
        None
    }
    /// Get device name
    fn name(&self) -> &'static str;
}

/// PCI address (bus, device, function)
#[derive(Debug, Clone, Copy)]
pub struct PciAddress {
    /// Bus number
    pub bus: u8,
    /// Device number
    pub device: u8,
    /// Function number
    pub function: u8,
}

impl PciAddress {
    /// Create a new PCI address
    pub const fn new(bus: u8, device: u8, function: u8) -> Self {
        Self { bus, device, function }
    }

    /// Convert PCI address to config space address
    pub fn config_addr(self, offset: u8) -> u32 {
        0x8000_0000
            | ((self.bus as u32) << 16)
            | ((self.device as u32) << 11)
            | ((self.function as u32) << 8)
            | ((offset as u32) & 0xFC)
    }
}

/// PCI device info from kernel
#[derive(Debug, Clone)]
pub struct PciDeviceInfo {
    /// PCI address (bus, device, function)
    pub addr: (u8, u8, u8),
    /// PCI vendor ID
    pub vendor_id: u16,
    /// PCI device ID
    pub device_id: u16,
    /// PCI class code
    pub class_code: u16,
    /// PCI subclass
    pub subclass: u8,
    /// PCI programming interface
    pub prog_if: u8,
    /// PCI revision
    pub revision: u8,
    /// BAR addresses
    pub bar: Vec<u64>,
    /// BAR sizes
    pub bar_size: Vec<u64>,
    /// IRQ line
    pub irq_line: u8,
    /// IRQ pin
    pub irq_pin: u8,
    /// MSI capability offset
    pub msi_cap_offset: Option<u8>,
    /// MSI-X capability offset
    pub msix_cap_offset: Option<u8>,
    /// PCIe capability offset
    pub pcie_cap_offset: Option<u8>,
}

/// PCI config space access - implemented by kernel
extern "Rust" {
    /// Read PCI config space (32-bit)
    pub fn pci_read_config32(bus: u8, device: u8, function: u8, offset: u8) -> u32;
    /// Write PCI config space (32-bit)
    pub fn pci_write_config32(bus: u8, device: u8, function: u8, offset: u8, value: u32);
    /// Find PCI devices by class code
    pub fn pci_find_class(class_code: u16) -> Vec<PciDeviceInfo>;
}

/// Log level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LogLevel {
    /// Debug level
    Debug = 0,
    /// Info level
    Info = 1,
    /// Warn level
    Warn = 2,
    /// Error level
    Error = 3,
    /// Critical level
    Critical = 4,
}

/// Logging functions - implemented by kernel
extern "Rust" {
    /// Log a message at debug level
    pub fn log_debug(msg: &str);
    /// Log a message at info level
    pub fn log_info(msg: &str);
    /// Log a message at warn level
    pub fn log_warn(msg: &str);
    /// Log a message at error level
    pub fn log_error(msg: &str);
    /// Log a message at critical level
    pub fn log_critical(msg: &str);
}

/// Log a debug-level message using `format!`-style arguments.
#[macro_export]
macro_rules! kdebug {
    ($($arg:tt)*) => { unsafe { $crate::log_debug(&alloc::format!($($arg)*)) } };
}

/// Log an info-level message using `format!`-style arguments.
#[macro_export]
macro_rules! kinfo {
    ($($arg:tt)*) => { unsafe { $crate::log_info(&alloc::format!($($arg)*)) } };
}

/// Log a warning-level message using `format!`-style arguments.
#[macro_export]
macro_rules! kwarn {
    ($($arg:tt)*) => { unsafe { $crate::log_warn(&alloc::format!($($arg)*)) } };
}

/// Log an error-level message using `format!`-style arguments.
#[macro_export]
macro_rules! kerror {
    ($($arg:tt)*) => { unsafe { $crate::log_error(&alloc::format!($($arg)*)) } };
}

/// Log a critical-level message using `format!`-style arguments.
#[macro_export]
macro_rules! kcritical {
    ($($arg:tt)*) => { unsafe { $crate::log_critical(&alloc::format!($($arg)*)) } };
}

/// Current timer tick count (safe wrapper over the kernel `extern` function).
pub fn ticks() -> u64 {
    unsafe { time_ticks() }
}

/// Timer frequency in ticks per second (safe wrapper).
pub fn timer_hz() -> u64 {
    unsafe { time_timer_hz() }
}

/// File type classification for a vnode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum NodeKind {
    /// Regular file with byte-addressable contents.
    File = 0,
    /// Directory containing named children.
    Dir = 1,
    /// Character device (byte stream).
    CharDevice = 2,
    /// FIFO/pipe (byte stream with EOF and blocking semantics).
    Pipe = 3,
    /// Symbolic link: a node whose contents are a path, resolved by the path
    /// walker rather than read by the caller.
    ///
    /// This is the last value, appended to the ABI's node-kind numbering, and it
    /// has to stay that way: the discriminant is passed to user space as
    /// `st_mode`'s type bits, so inserting a variant would re-type every
    /// existing node. Append, as `docs/ABI.md` requires of the syscall table.
    Symlink = 4,
}

impl core::fmt::Display for NodeKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NodeKind::File => write!(f, "file"),
            NodeKind::Dir => write!(f, "dir"),
            NodeKind::CharDevice => write!(f, "char"),
            NodeKind::Pipe => write!(f, "pipe"),
            NodeKind::Symlink => write!(f, "symlink"),
        }
    }
}

/// Filesystem error space. Values map 1:1 onto ABI errnos when returned
/// through syscalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum FsError {
    /// Path component does not exist.
    NotFound = -2,
    /// Path exists but the operation needs something else.
    Exists = -17,
    /// Operation not supported by this vnode.
    NotSupported = -38,
    /// Not a directory where one was required.
    NotADirectory = -20,
    /// Is a directory where a file was required.
    IsADirectory = -21,
    /// Descriptor table full or bad fd.
    BadDescriptor = -9,
    /// Permission denied.
    Permission = -1,
    /// Permission denied for an access mode (POSIX `EACCES`).
    AccessDenied = -13,
    /// Invalid argument (POSIX `EINVAL`).
    Invalid = -22,
    /// No buffer space / queue empty after non-blocking check.
    WouldBlock = -11,
    /// No space left (file size cap or storage exhausted).
    OutOfSpace = -28,
    /// Input/output error talking to the underlying device or driver.
    IoError = -5,
    /// Illegal seek (POSIX `ESPIPE`): the descriptor has no meaningful cursor,
    /// so `lseek(2)` cannot answer. Distinct from [`FsError::NotSupported`]
    /// (`ENOSYS`), which says "this filesystem never grew that feature";
    /// `ESPIPE` is what a *stream* legitimately reports and what stdio uses to
    /// decide a stream is not seekable.
    IllegalSeek = -29,
    /// A writer's counterparty (every reader end) went away.
    BrokenPipe = -32,
    /// Blocking operation interrupted by a pending process signal (POSIX
    /// `EINTR`); syscalls may restart it if the handler requests `SA_RESTART`.
    Interrupted = -4,
    /// Too many symbolic links followed (POSIX `ELOOP`).
    ///
    /// Distinct from `NotSupported` because it is not a missing feature. A path
    /// that loops is a *path* problem, and a caller that gets `ENOSYS` will
    /// reasonably conclude the symlink is not what it expected and go looking
    /// elsewhere; a caller that gets `ELOOP` knows its own link chain is wrong.
    /// -1 on the wire, which is `ELOOP`'s value.
    TooManyLinks = -40,
}

/// Convenience: is this `EINTR`?
impl FsError {
    /// True when the operation was interrupted by a signal.
    pub fn is_interrupted(self) -> bool {
        self == FsError::Interrupted
    }
}

impl From<FsError> for i64 {
    fn from(e: FsError) -> i64 {
        e as i64
    }
}

impl From<FsError> for i32 {
    fn from(e: FsError) -> i32 {
        e as i32
    }
}

/// POSIX permission-mode bit constants (the low 12 bits of `st_mode`).
pub const S_ISUID: u32 = 0o4000;
/// Set-group-ID on execution / group inheritance.
pub const S_ISGID: u32 = 0o2000;
/// Sticky bit.
pub const S_ISVTX: u32 = 0o1000;
/// Owner read.
pub const S_IRUSR: u32 = 0o400;
/// Owner write.
pub const S_IWUSR: u32 = 0o200;
/// Owner execute.
pub const S_IXUSR: u32 = 0o100;
/// Group read.
pub const S_IRGRP: u32 = 0o040;
/// Group write.
pub const S_IWGRP: u32 = 0o020;
/// Group execute.
pub const S_IXGRP: u32 = 0o010;
/// Other read.
pub const S_IROTH: u32 = 0o004;
/// Other write.
pub const S_IWOTH: u32 = 0o002;
/// Other execute.
pub const S_IXOTH: u32 = 0o001;
/// Owner read/write/execute.
pub const S_IRWXU: u32 = S_IRUSR | S_IWUSR | S_IXUSR;
/// Group read/write/execute.
pub const S_IRWXG: u32 = S_IRGRP | S_IWGRP | S_IXGRP;
/// Other read/write/execute.
pub const S_IRWXO: u32 = S_IROTH | S_IWOTH | S_IXOTH;
/// All permission bits (plus setuid/setgid/sticky).
pub const S_PERM_MASK: u32 = 0o7777;
/// The nine read/write/execute bits.
pub const S_ACC_MASK: u32 = 0o777;

/// `poll` event: data can be read without blocking (a pipe at EOF also sets
/// `POLLIN`, exactly as a read that returns `0`).
pub const POLLIN: u16 = 0x001;
/// `poll` event: data can be written without blocking.
pub const POLLOUT: u16 = 0x004;
/// `poll` event: exceptional condition (a pipe write with no reader sets this
/// in addition to never granting `POLLOUT`).
pub const POLLERR: u16 = 0x008;
/// `poll` event: the stream was hung up (a pipe read end whose last writer
/// closed sets this alongside `POLLIN`/EOF).
pub const POLLHUP: u16 = 0x010;
/// `poll` event: the descriptor is not open in the calling process. Never
/// settable by a vnode; reported by the descriptor table.
pub const POLLNVAL: u16 = 0x020;

/// Dynamic vnode handle shared across users of a file.
pub type VnodeRef = Arc<dyn Vnode>;

/// The operations every vnode provides. Default implementations return
/// `FsError::NotSupported` so leaf implementations stay minimal.
pub trait Vnode: Send + Sync {
    /// Kind of this node.
    fn kind(&self) -> NodeKind;
    /// Read bytes at `offset`; returns bytes read.
    fn read_at(&self, _offset: u64, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::NotSupported)
    }
    /// Write bytes at `offset`; returns bytes written.
    fn write_at(&self, _offset: u64, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::NotSupported)
    }
    /// Execute a device control request. `data` is a kernel-owned copy of the
    /// ioctl argument, never an unchecked userspace pointer.
    fn ioctl(&self, _cmd: u32, _data: &mut [u8]) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }
    /// Whether this node is a terminal: it carries termios state, echoes and
    /// edits input, and can deliver `ISIG` signals to a foreground process
    /// group.
    ///
    /// This is the kernel's own answer to `isatty`. It governs two behaviors
    /// that a libc relies on and cannot discover any other way:
    ///
    ///  * a blocking `read` on a terminal waits for input instead of
    ///    reporting end-of-file, which is what a stdio read loop needs;
    ///  * `isatty` is true here, so stdio picks line buffering and emits
    ///    escapes.
    ///
    /// Userspace still learns the answer the portable way — `TCGETS`
    /// succeeding — which falls out of this for free because only terminals
    /// implement that request.
    fn is_terminal(&self) -> bool {
        false
    }
    /// Claim this terminal for session `sid` if it has no owner yet. See
    /// [`CharDevice::acquire_session`]; only terminals do anything with it.
    fn acquire_session(&self, _sid: u32) {}
    /// Resolve `name` inside this directory.
    fn lookup(&self, _name: &str) -> Result<VnodeRef, FsError> {
        Err(FsError::NotSupported)
    }
    /// Create a new child of this directory.
    ///
    /// `uid`/`gid` are the new node's owner and `mode` its permission bits
    /// (see the `S_*` constants). Implementations apply them at construction
    /// so the metadata exists before the caller ever observes the node.
    fn create_child(
        &self,
        _name: &str,
        _kind: NodeKind,
        _uid: u32,
        _gid: u32,
        _mode: u32,
    ) -> Result<VnodeRef, FsError> {
        Err(FsError::NotSupported)
    }
    /// Remove the child called `name`, if this is a directory that has one.
    ///
    /// The counterpart to [`Vnode::create_child`]. The default refuses, which is
    /// the right answer for every node type that has no name-space (character
    /// devices, pipes) and for filesystems that do not implement removal.
    fn remove_child(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }
    /// Publish an *existing* node under a new name in this directory.
    ///
    /// This is what `rename(2)` and `link(2)` are made of: neither creates
    /// anything, they move an existing node's name from one directory to
    /// another, or give it a second name. Keeping the node and moving only the
    /// entry is what makes a rename atomic from a reader's point of view, and
    /// what makes a hard link share one inode rather than copy the file.
    ///
    /// A filesystem that cannot express this -- one where every path is a
    /// distinct object -- leaves the default.
    fn attach_child(&self, _name: &str, _node: VnodeRef) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }
    /// Permission mode bits (see the `S_*` constants). `0` = no metadata.
    fn mode(&self) -> u32 {
        0
    }
    /// Owner user id.
    fn uid(&self) -> u32 {
        0
    }
    /// Owner group id.
    fn gid(&self) -> u32 {
        0
    }
    /// Replace the permission mode bits.
    fn set_mode(&self, _mode: u32) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }
    /// Replace the owner uid/gid. An argument of `u32::MAX` leaves that
    /// component unchanged (mirrors the `chown` ABI).
    fn set_owner(&self, _uid: u32, _gid: u32) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }
    /// Truncate contents to zero length.
    fn truncate(&self) -> Result<(), FsError> {
        Err(FsError::NotSupported)
    }
    /// Physical range backing this node, for `mmap(2)` on a device file.
    ///
    /// `Some((phys_base, len))` means the node is a window onto physical memory
    /// and a process may map it directly. The default is `None`: a node that is
    /// not a device range must be read and written through `read_at` /
    /// `write_at` instead, and guessing a range for a regular file would let a
    /// process map kernel RAM.
    ///
    /// Only device nodes should answer this. It is what lets `/dev/fb0` work
    /// for a program that expects to mmap the framebuffer, which is how most
    /// terminals reach a linear framebuffer -- a per-pixel `write(2)` is orders
    /// of magnitude too slow to redraw a screen.
    fn mmap_phys(&self) -> Option<(u64, u64)> {
        None
    }
    /// List children as `(name, kind)` pairs.
    fn list(&self) -> Result<Vec<(String, NodeKind)>, FsError> {
        Err(FsError::NotSupported)
    }
    /// The path this node points at, if it is a symbolic link.
    ///
    /// A trait method rather than a read of `read_at` because a symlink's target
    /// is *not* file content. Reading it through `read_at` would make the target
    /// readable, writable and truncatable like any other byte range, and every
    /// caller of `readlink(2)` would then be asking whether the bytes they read
    /// happen to end in a NUL. Keeping it separate also means the path walker
    /// does not have to allocate a buffer to learn where to go next.
    ///
    /// The default is `None`, meaning "not a symbolic link", which is the right
    /// answer for every node type that has no target.
    fn symlink_target(&self) -> Option<String> {
        None
    }
    /// Create a child symbolic link called `name` pointing at `target`.
    ///
    /// The counterpart to [`Vnode::symlink_target`], kept beside it so that a
    /// filesystem implementing one and not the other is visibly incomplete. The
    /// target is stored verbatim, relative or absolute: resolution against the
    /// link's own directory is the path walker's job, and a filesystem that
    /// stored it pre-resolved would be wrong the moment the link were moved.
    fn create_symlink(
        &self,
        _name: &str,
        _target: &str,
        _uid: u32,
        _gid: u32,
        _mode: u32,
    ) -> Result<VnodeRef, FsError> {
        Err(FsError::NotSupported)
    }
    /// Non-blocking readability probe used by device nodes.
    fn readable_now(&self) -> bool {
        true
    }
    /// Non-blocking writability probe. `true` = a write would not block.
    fn writable_now(&self) -> bool {
        true
    }
    /// Readiness snapshot for `poll(2)`-style multiplexing. Returns the event
    /// bits that are currently satisfiable. The default derives from
    /// [`readable_now`] / [`writable_now`]; vnodes with richer conditions
    /// (pipes: EOF, space, EPIPE) override this.
    fn poll_events(&self, _interest: u16) -> u16 {
        let mut r = 0;
        if self.readable_now() {
            r |= POLLIN;
        }
        if self.writable_now() {
            r |= POLLOUT;
        }
        r
    }
    /// Register `task` for a readiness wakeup (under this node's lock, so a
    /// concurrent state change cannot be missed). Returns `true` if the node
    /// is already ready and `false` once `task` is registered.
    ///
    /// The default registers nothing: for truly-ready nodes the caller never
    /// parks, and the fallback is a timer-driven re-poll. Event-driven vnodes
    /// (pipes, byte-queue devices) override this to wake `task` from the
    /// `POLL_*` transition they trigger on their next state change.
    fn poll_park(&self, _task: usize, interest: u16) -> bool {
        // Only report ready for a direction the caller actually asked about.
        //
        // This is not a refinement, it is the difference between working and
        // spinning forever. A node that answers "I am ready for *something*"
        // to a caller that asked about one direction will say yes to a
        // descriptor that is ready the wrong way, the caller re-probes, gets
        // the same answer, and never parks -- a livelock at 100% CPU that looks
        // exactly like a hang. Asking a pipe's read end whether it is writable
        // is the case that exposes it: the end is legitimately ready to *read*
        // and never ready to write.
        let mut ready = 0u16;
        if interest & POLLIN != 0 && self.readable_now() {
            ready |= POLLIN;
        }
        if interest & POLLOUT != 0 && self.writable_now() {
            ready |= POLLOUT;
        }
        ready & interest != 0
    }
    /// Cancel a previously registered poll waiter; idempotent.
    fn poll_cancel(&self, _task: usize) {}
    /// Called when a new open file description starts referencing this node
    /// (a descriptor install, or a descriptor-table copy during `fork`).
    ///
    /// Vnodes that track how many instances are open (pipes, for EOF and
    /// `EPIPE` semantics) bump their counters here. The default is a no-op.
    fn on_open(&self) {}
    /// Called when an open file description referencing this node is released
    /// (`close`, or descriptor-table teardown).
    ///
    /// Vnodes tracking open instances decrement their counters here and wake
    /// waiters whose condition (data / space / counterpart) may have changed.
    /// The default is a no-op.
    fn on_close(&self) {}
    /// Human-readable size for listings.
    fn size_hint(&self) -> u64 {
        0
    }
    /// Whether `lseek(2)` can move this node's cursor.
    ///
    /// False for anything that is a byte *stream* rather than a file -- pipes,
    /// character devices, the console. The distinction matters to user space,
    /// not just to us: stdio calls `lseek` to find out whether a stream can be
    /// repositioned and picks its buffering strategy from the answer, so a
    /// stream that claimed to be seekable would let a program seek and then
    /// silently read the wrong bytes.
    fn is_seekable(&self) -> bool {
        false
    }
    /// Current length in bytes, as `lseek(fd, 0, SEEK_END)` must report.
    ///
    /// Only meaningful when [`Vnode::is_seekable`] is true.
    fn file_size(&self) -> u64 {
        0
    }
    /// Stable per-filesystem identity of this node, reported as `st_ino`.
    ///
    /// User space genuinely needs this: a shell's tab completion deduplicates
    /// by `(st_dev, st_ino)`, `cp -l` hard-links on it, and `find` uses it to
    /// avoid walking into the same directory twice. Returning a constant would
    /// make all files look like one file, so the default is distinct-per-node
    /// where the implementation can arrange it and 0 only when it genuinely
    /// cannot.
    fn inode(&self) -> u64 {
        0
    }
    /// Which filesystem this node lives on, reported as `st_dev`.
    ///
    /// Together with [`Vnode::inode`] this is a file's identity: two nodes
    /// with the same pair are the same file reachable by two names, which is
    /// what a hard link *is*.
    fn device(&self) -> u32 {
        0
    }
    /// Seconds and nanoseconds since the Unix epoch at which this node's
    /// contents last changed (`st_mtime`).
    ///
    /// `None` means "this node has no meaningful timestamp" -- a character
    /// device or a pipe. A node that does have one should not return the epoch:
    /// `ls -l` would then show 1970 and a build system comparing timestamps
    /// would treat every output as older than every input.
    fn mtime(&self) -> Option<(i64, u32)> {
        None
    }
}

/// VFS functions - implemented by kernel
extern "Rust" {
    /// Resolve a path to a vnode
    pub fn vfs_resolve(path: &str) -> Result<VnodeRef, i32>;
    /// Mount a filesystem at a path
    pub fn vfs_mount(path: &str, root: VnodeRef) -> Result<(), i32>;
    /// Create a vnode at path
    pub fn vfs_create(path: &str, kind: NodeKind) -> Result<VnodeRef, i32>;
    /// Register a character device
    pub fn vfs_devfs_register(name: &str, dev: *mut ()) -> Result<(), i32>;
    /// Register a block device
    pub fn vfs_devfs_register_block(name: &str, dev: *mut ()) -> Result<(), i32>;
    /// Register a character device into `/dev/<dir>/<name>`, creating `dir` if
    /// it does not exist. Used for `/dev/pts/<n>`.
    pub fn vfs_devfs_register_in_dir(dir: &str, name: &str, dev: *mut ()) -> Result<(), i32>;
}

/// Register a character device into a devfs subdirectory, e.g. `/dev/pts/3`.
///
/// Separate from [`register_char_device`] because the Linux pty layout puts
/// slaves in a subdirectory, and a program that has been handed `/dev/pts/3` by
/// `ptsname(3)` has to be able to open exactly that. A flat name would leave
/// `ptsname` reporting a path that does not exist.
pub fn register_char_device_in_dir(
    dir: &str,
    name: &str,
    dev: Arc<dyn CharDevice>,
) -> DriverResult<()> {
    let raw = Box::into_raw(Box::new(dev)) as *mut ();
    let res = unsafe { vfs_devfs_register_in_dir(dir, name, raw) };
    if res.is_err() {
        // SAFETY: `raw` came from `Box::into_raw` of a `Box<Arc<dyn CharDevice>>`.
        unsafe {
            let _ = *Box::from_raw(raw as *mut Arc<dyn CharDevice>);
        }
        Err(DriverError::IoError)
    } else {
        Ok(())
    }
}

/// Register a character device instance with the kernel devfs.
///
/// The `Arc<dyn CharDevice>` is moved across the kernel boundary; on failure
/// the reference is reclaimed to avoid leaking it.
pub fn register_char_device(name: &str, dev: Arc<dyn CharDevice>) -> DriverResult<()> {
    let raw = Box::into_raw(Box::new(dev)) as *mut ();
    let res = unsafe { vfs_devfs_register(name, raw) };
    if res.is_err() {
        // SAFETY: `raw` came from `Box::into_raw` of a `Box<Arc<dyn CharDevice>>`.
        unsafe {
            let _ = *Box::from_raw(raw as *mut Arc<dyn CharDevice>);
        }
        Err(DriverError::IoError)
    } else {
        Ok(())
    }
}

/// Register a block device instance with the kernel devfs.
///
/// The `Arc<dyn BlockDevice>` is moved across the kernel boundary; on failure
/// the reference is reclaimed to avoid leaking it.
pub fn register_block_device(name: &str, dev: Arc<dyn BlockDevice>) -> DriverResult<()> {
    let raw = Box::into_raw(Box::new(dev)) as *mut ();
    let res = unsafe { vfs_devfs_register_block(name, raw) };
    if res.is_err() {
        // SAFETY: `raw` came from `Box::into_raw` of a `Box<Arc<dyn BlockDevice>>`.
        unsafe {
            let _ = *Box::from_raw(raw as *mut Arc<dyn BlockDevice>);
        }
        Err(DriverError::IoError)
    } else {
        Ok(())
    }
}

/// Time functions - implemented by kernel
extern "Rust" {
    /// Get current tick count
    pub fn time_ticks() -> u64;
    /// Get timer frequency (ticks per second)
    pub fn time_timer_hz() -> u64;
}
