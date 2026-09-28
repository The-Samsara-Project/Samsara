// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! User-space memory grants.
//!
//! The kernel hands physical pages to user processes only through explicit
//! grants, mirroring a capability model:
//!
//! * [`map_anon`] — fresh zeroed RAM mapped into the caller's address space
//!   (used as the backing for user heaps and per-process data).
//! * [`map_phys`] — a *device* physical range mapped for MMIO. Only ranges
//!   previously registered by the kernel as device memory may be mapped, so
//!   user drivers cannot poke at kernel-owned RAM.
//! * [`dma_alloc`] / [`dma_free`] — physically contiguous memory for DMA,
//!   mapped into the driving process; freed back to the kernel on release.
//! * [`shm_create`] / [`shm_map`] / [`shm_destroy`] — physically *shared*
//!   memory: a region created by one process and mapped by any peer that knows
//!   its handle. This is the transport the window compositor uses to read
//!   client surfaces without copying pixels through IPC.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

// `mprotect(2)` protection bits. These are Linux's values, which is the point:
// a program computing them from its own `<sys/mman.h>` lands on the same
// numbers the kernel expects.
const PROT_NONE: u32 = 0;
const PROT_READ: u32 = 1;
const PROT_WRITE: u32 = 2;
const PROT_EXEC: u32 = 4;

use super::{pmm, phys_to_virt, vmm};
use crate::sync::Spinlock;

/// Frame size for mapping arithmetic.
use pmm::FRAME_SIZE;

/// Kind of a user mapping grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantKind {
    /// Freshly allocated anonymous RAM.
    Anon,
    /// Hardware device MMIO exposed through a kernel-registered range.
    Device,
    /// Physically contiguous DMA memory.
    Dma,
    /// Physically shared memory (see the `SHM_*` family).
    Shared,
}

/// One granted mapping tracked for revocation.
#[derive(Debug, Clone, Copy)]
pub struct Mapping {
    kind: GrantKind,
    phys_base: usize,
    frames: usize,
    /// Task id that established this grant (lets `SHM_DESTROY` find the
    /// caller's own shared mapping; 0 = kernel bootstrap, never issued).
    owner: u64,
    /// SHM region handle for [`GrantKind::Shared`], `0` otherwise.
    handle: u64,
}

/// Task id of the running process, or 0 when no user task is current (kernel
/// bootstrap / scheduler context).
fn cur_id() -> u64 {
    crate::task::sched::current_task_id()
        .map(|t| t.0 as u64)
        .unwrap_or(0)
}

/// Base virtual address cursor for anonymous grants (grows upward in the
/// user half). The low 1 GiB text/stack region is left untouched.
static NEXT_ANON_VA: AtomicUsize = AtomicUsize::new(0x0000_4000_0000);

/// Registry of active grants (keyed by user virtual base).
static MAPPINGS: Spinlock<BTreeMap<usize, Mapping>> = Spinlock::new(BTreeMap::new());

/// Physical ranges the kernel authorizes for MMIO mapping: `(start, size)`.
static DEVICE_REGIONS: Spinlock<Vec<(u64, u64)>> = Spinlock::new(Vec::new());

/// Register a physical range as authorized device memory (called by the PCI
//// enumerator as BARs are discovered).
pub fn register_device_region(start: u64, size: u64) {
    if size == 0 {
        return;
    }
    DEVICE_REGIONS.lock().push((start, size));
}

/// Whether `[phys, phys + frames*FRAME_SIZE)` lies wholly inside an
/// authorized device range.
fn device_range_authorized(phys: u64, frames: usize) -> bool {
    let end = phys + (frames as u64) * FRAME_SIZE as u64;
    DEVICE_REGIONS.lock().iter().any(|&(s, sz)| {
        phys >= s && end <= s + sz && phys.checked_add(end - phys).is_some()
    })
}

/// Reserve a fresh virtual region for the current process.
fn alloc_va(frames: usize) -> Option<usize> {
    let va = NEXT_ANON_VA.fetch_add(frames * FRAME_SIZE, Ordering::Relaxed);
    Some(va)
}

/// Flush the TLB for `frames` pages starting at `va`.
fn flush(va: usize, frames: usize) {
    for i in 0..frames {
        vmm::invlpg(va + i * FRAME_SIZE);
    }
}

/// Record a grant so it can be revoked/accounted later.
fn record(va: usize, phys: usize, frames: usize, kind: GrantKind, owner: u64, handle: u64) {
    MAPPINGS.lock().insert(
        va,
        Mapping {
            kind,
            phys_base: phys,
            frames,
            owner,
            handle,
        },
    );
}

/// Look up the grant for `va`.
fn lookup(va: usize) -> Option<Mapping> {
    MAPPINGS.lock().get(&va).copied()
}

/// Remove and return the grant for `va`.
fn take(va: usize) -> Option<Mapping> {
    MAPPINGS.lock().remove(&va)
}

/// Map `frames` freshly allocated, zeroed anonymous pages into the current
/// process's address space. Returns the virtual base, or `None` on failure.
pub fn map_anon(frames: usize) -> Option<usize> {
    if frames == 0 {
        return None;
    }
    let as_root = crate::task::sched::current_as_root()?;
    let mut aspace = vmm::AddressSpace::from_root(as_root);
    let va = alloc_va(frames)?;

    let flags = vmm::USER_ACCESSIBLE | vmm::WRITABLE | vmm::NO_EXECUTE;
    let mut phys_base = 0usize;
    for i in 0..frames {
        let phys = pmm::alloc_frame()?;
        if i == 0 {
            phys_base = phys;
        }
        // Zero the fresh frame before it becomes reachable.
        unsafe {
            core::ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, FRAME_SIZE);
            aspace.map_page(va + i * FRAME_SIZE, phys, flags);
            vmm::invlpg(va + i * FRAME_SIZE);
        }
    }
    record(va, phys_base, frames, GrantKind::Anon, cur_id(), 0);
    crate::log::kdebug!(
        "umem: anon {} frames at {:#x}",
        frames,
        va
    );
    Some(va)
}

/// Remove an anonymous mapping owned by the current process.
///
/// Ownership is checked, and it has to be: `MAPPINGS` is a flat table shared by
/// every process, and a mapping belongs to the one that created it. Without the
/// check, a process could name another process's virtual address and unmap it,
/// which is a cross-process denial of service reachable from ring 3.
///
/// Returns the number of frames released, or an errno. A caller that unmaps a
/// region which is not a single whole mapping is refused (`EINVAL`) rather than
/// rounding outwards: the kernel is not obliged to be forgiving here, and
/// guessing would let a program unmap memory it did not map.
pub fn unmap_anon(va: usize, size: usize) -> Result<usize, i64> {
    if size == 0 {
        return Err(crate::abi::errno::EINVAL);
    }
    let m = match lookup(va) {
        Some(m) => m,
        None => return Err(crate::abi::errno::EINVAL),
    };
    if m.kind != GrantKind::Anon {
        return Err(crate::abi::errno::EINVAL);
    }
    if m.owner != cur_id() {
        return Err(crate::abi::errno::EPERM);
    }
    // Only the exact mapping may be removed. `munmap` is specified to unmap
    // whole pages, and this kernel hands out page-aligned regions, so a length
    // that is not a multiple of the frame size -- or that does not cover the
    // mapping exactly -- is a caller mistake.
    if size != m.frames * FRAME_SIZE {
        return Err(crate::abi::errno::EINVAL);
    }
    let as_root = match crate::task::sched::current_as_root() {
        Some(r) => r,
        None => return Err(crate::abi::errno::EPERM),
    };
    let mut aspace = vmm::AddressSpace::from_root(as_root);
    for i in 0..m.frames {
        let a = va + i * FRAME_SIZE;
        // SAFETY: these are the frames this process mapped and is now giving
        // back; the VA range was recorded by `map_anon` for this owner.
        unsafe {
            aspace.unmap_page(a);
            vmm::invlpg(a);
        }
    }
    // Recycle the frames only after they are unreachable, so a stray access
    // during the window faults rather than reading somebody else's page.
    for i in 0..m.frames {
        pmm::free_frame(m.phys_base + i * FRAME_SIZE);
    }
    take(va);
    Ok(m.frames)
}

/// Change the protection of an anonymous mapping owned by the current process.
///
/// This is the difference between "a mapping is always writable" and "a program
/// can make a page read-only, and then fault on write". Without it, every
/// mapping stays RW, so a `mprotect` to `PROT_READ` is silently a no-op and
/// nothing that relies on the page becoming immutable can work.
///
/// `prot` is translated to the same flags `map_anon` uses, so the result is
/// indistinguishable from a mapping created with those protections.
pub fn protect_anon(va: usize, size: usize, prot: u32) -> Result<(), i64> {
    let m = match lookup(va) {
        Some(m) => m,
        None => return Err(crate::abi::errno::ENOMEM),
    };
    if m.kind != GrantKind::Anon {
        return Err(crate::abi::errno::ENOMEM);
    }
    if m.owner != cur_id() {
        return Err(crate::abi::errno::EPERM);
    }
    if size != m.frames * FRAME_SIZE {
        return Err(crate::abi::errno::EINVAL);
    }
    // Reject a request the kernel cannot express rather than silently rounding
    // it. An unknown bit set would otherwise become "readable" and read as
    // success for something the caller did not ask for.
    let known = PROT_READ | PROT_WRITE | PROT_EXEC;
    if prot & !known != 0 {
        return Err(crate::abi::errno::EINVAL);
    }
    // An inaccessible mapping has no flags, so a `PROT_NONE` page cannot be
    // represented by clearing bits alone -- it needs the user bit gone too.
    if prot == 0 {
        return Err(crate::abi::errno::ENOSYS);
    }
    let as_root = match crate::task::sched::current_as_root() {
        Some(r) => r,
        None => return Err(crate::abi::errno::EPERM),
    };
    let mut aspace = vmm::AddressSpace::from_root(as_root);
    let mut flags = vmm::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        flags |= vmm::WRITABLE;
    }
    if prot & PROT_EXEC == 0 {
        flags |= vmm::NO_EXECUTE;
    }
    for i in 0..m.frames {
        let a = va + i * FRAME_SIZE;
        // SAFETY: the range belongs to this process and `m.phys_base` is the
        // base of exactly `m.frames` frames recorded at `map_anon` time.
        unsafe {
            aspace.remap_page(a, m.phys_base + i * FRAME_SIZE, flags);
            vmm::invlpg(a);
        }
    }
    Ok(())
}

/// Map an authorized device-Memory range into the current process (MMIO for
/// user-space drivers). Returns the virtual base, or `None` if the range is
/// not authorized.
pub fn map_phys(phys: u64, frames: usize, flags: u64) -> Option<usize> {
    if frames == 0 || !device_range_authorized(phys, frames) {
        return None;
    }
    let as_root = crate::task::sched::current_as_root()?;
    let mut aspace = vmm::AddressSpace::from_root(as_root);
    let va = alloc_va(frames)?;
    // SAFETY: the range is registered device memory, never kernel RAM, and
    // the destination VA is freshly reserved.
    unsafe {
        aspace.map_contiguous(va, phys as usize, frames, flags | vmm::USER_ACCESSIBLE);
        for i in 0..frames {
            vmm::invlpg(va + i * FRAME_SIZE);
        }
    }
    record(va, phys as usize, frames, GrantKind::Device, cur_id(), 0);
    Some(va)
}

/// Allocate `frames` physically-contiguous DMA pages mapped into the current
/// process. Returns the virtual base (the physical base is recoverable via
/// [`dma_to_phys`]).
pub fn dma_alloc(frames: usize) -> Option<usize> {
    if frames == 0 {
        return None;
    }
    let phys = pmm::alloc_dma_pages(frames)? as usize;
    let as_root = crate::task::sched::current_as_root()?;
    let va = alloc_va(frames)?;
    let flags = vmm::USER_ACCESSIBLE | vmm::WRITABLE | vmm::NO_EXECUTE;
    let mut aspace = vmm::AddressSpace::from_root(as_root);
    // SAFETY: DMA frames are dedicated to this process; VA is freshly chosen.
    unsafe {
        aspace.map_contiguous(va, phys, frames, flags);
        for i in 0..frames {
            vmm::invlpg(va + i * FRAME_SIZE);
        }
    }
    record(va, phys, frames, GrantKind::Dma, cur_id(), 0);
    crate::log::kdebug!("umem: dma {} frames at {:#x}", frames, va);
    Some(va)
}

/// Physical base of a DMA grant previously returned by [`dma_alloc`].
pub fn dma_to_phys(va: usize) -> Option<u64> {
    lookup(va).filter(|m| m.kind == GrantKind::Dma).map(|m| m.phys_base as u64)
}

/// Release a DMA grant: unmap, free the pages and forget the mapping.
pub fn dma_free(va: usize, frames: usize) -> Result<(), ()> {
    let m = take(va).ok_or(())?;
    if m.kind != GrantKind::Dma || m.frames < frames {
        return Err(());
    }
    let mut aspace = vmm::AddressSpace::from_root(
        crate::task::sched::current_as_root().ok_or(())?,
    );
    for i in 0..frames {
        // SAFETY: mapping is owned by this grant; TLB flushed below.
        let phys = unsafe { aspace.unmap_page(va + i * FRAME_SIZE) };
        if let Some(p) = phys {
            pmm::free_frame(p);
        }
        vmm::invlpg(va + i * FRAME_SIZE);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared memory (SHM_CREATE / SHM_MAP / SHM_DESTROY)
// ---------------------------------------------------------------------------

/// Cap on one shared region, in 4 KiB frames.
const MAX_SHM_FRAMES: usize = 1 << 16;
/// Cap on live shared regions (slots are recycled after destruction).
const MAX_SHM_REGIONS: usize = 256;

/// One live shared-memory region. `phys` lists one physical frame per page in
/// order; frames are deliberately *not* contiguous, so surfaces never depend
/// on the (small) DMA pool.
struct SharedRegion {
    handle: u64,
    phys: Vec<usize>,
    /// Number of live mapping sets: one per `SHM_CREATE`, one per `SHM_MAP`
    /// and one per fork clone that re-maps the region. Frames are freed when
    /// this reaches zero.
    refs: u32,
}

static SHM_REGIONS: Spinlock<Vec<Option<SharedRegion>>> = Spinlock::new(Vec::new());

/// Monotonic handle source; a handle is a local, capability-like token shared
/// out-of-band between cooperating processes.
static NEXT_SHM_HANDLE: AtomicUsize = AtomicUsize::new(0x5A5A_0000);

/// Allocate `frames` fresh physical pages for a shared region.
fn shm_alloc_frames(frames: usize) -> Option<Vec<usize>> {
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        match pmm::alloc_frame() {
            Some(p) => out.push(p),
            None => {
                for p in out {
                    pmm::free_frame(p);
                }
                return None;
            }
        }
    }
    Some(out)
}

/// Index of the slot holding `handle`, if the region is live.
fn shm_slot(handle: u64) -> Option<usize> {
    let g = SHM_REGIONS.lock();
    g.iter().position(|s| matches!(s, Some(r) if r.handle == handle))
}

/// Per-frame `phys -> handle` index over every live region.
fn shm_phys_to_handle() -> BTreeMap<usize, u64> {
    let mut out = BTreeMap::new();
    let g = SHM_REGIONS.lock();
    for s in g.iter().flatten() {
        for &p in &s.phys {
            out.insert(p, s.handle);
        }
    }
    out
}

/// Map the pages of `handle` into the current process and record the grant.
fn shm_map_pages(handle: u64, phys: &[usize]) -> Option<usize> {
    let as_root = crate::task::sched::current_as_root()?;
    let mut aspace = vmm::AddressSpace::from_root(as_root);
    let va = alloc_va(phys.len())?;
    let flags = vmm::USER_ACCESSIBLE | vmm::WRITABLE | vmm::NO_EXECUTE;
    // SAFETY: the frames belong to the region and the VAs are freshly
    // reserved; each page is flushed after mapping.
    unsafe {
        for (i, &p) in phys.iter().enumerate() {
            aspace.map_page(va + i * FRAME_SIZE, p, flags);
            vmm::invlpg(va + i * FRAME_SIZE);
        }
    }
    record(va, phys[0], phys.len(), GrantKind::Shared, cur_id(), handle);
    Some(va)
}

/// Unmap every one of *this* process's mappings of `handle` (found by owner +
/// handle) and forget their grants. Returns how many mappings were dropped so
/// the caller can mirror that into the region refcount.
fn shm_unmap_own(handle: u64) -> Result<usize, i64> {
    let owner = cur_id();
    if owner == 0 {
        return Err(crate::abi::errno::EPERM);
    }
    // A process may legitimately map a region several times (e.g. `SHM_CREATE`
    // maps internally, then the creator calls `SHM_MAP` for its own draw VA);
    // drop them all so bookkeeping never strands frames.
    let vms: Vec<(usize, usize)> = {
        let g = MAPPINGS.lock();
        g.iter()
            .filter(|(_, m)| {
                m.kind == GrantKind::Shared && m.handle == handle && m.owner == owner
            })
            .map(|(va, m)| (*va, m.frames))
            .collect()
    };
    if vms.is_empty() {
        return Err(crate::abi::errno::ENOENT);
    }
    let as_root = crate::task::sched::current_as_root().ok_or(crate::abi::errno::EPERM)?;
    let mut aspace = vmm::AddressSpace::from_root(as_root);
    for (va, frames) in &vms {
        // SAFETY: mappings are recorded as ours; TLB flushed after each page.
        unsafe {
            for i in 0..*frames {
                aspace.unmap_page(va + i * FRAME_SIZE);
                vmm::invlpg(va + i * FRAME_SIZE);
            }
        }
        take(*va);
    }
    Ok(vms.len())
}

/// Refcount one more mapping set of `handle` (fork clone accounting).
fn shm_retain(handle: u64) {
    if let Some(slot) = shm_slot(handle) {
        let mut g = SHM_REGIONS.lock();
        if let Some(r) = g.get_mut(slot).and_then(|s| s.as_mut()) {
            r.refs += 1;
        }
    }
}

/// Refcount `n` fewer mapping sets of `handle`; free the frames when the last
/// one goes away.
fn shm_release_n(handle: u64, n: u32) {
    if let Some(slot) = shm_slot(handle) {
        let mut g = SHM_REGIONS.lock();
        let dead = {
            let r = match g.get_mut(slot) {
                Some(Some(r)) => r,
                _ => return,
            };
            r.refs = r.refs.saturating_sub(n);
            r.refs == 0
        };
        if dead {
            if let Some(r) = g[slot].take() {
                for p in r.phys {
                    pmm::free_frame(p);
                }
            }
        }
    }
}

/// Create a shared-memory region of `frames` pages mapped into the caller.
/// Returns a handle peers can pass to [`shm_map`].
pub fn shm_create(frames: usize) -> Result<u64, i64> {
    if cur_id() == 0 {
        return Err(crate::abi::errno::EPERM);
    }
    if frames == 0 || frames > MAX_SHM_FRAMES {
        return Err(crate::abi::errno::EINVAL);
    }
    if SHM_REGIONS.lock().iter().filter(|s| s.is_some()).count() >= MAX_SHM_REGIONS {
        return Err(crate::abi::errno::ENOMEM);
    }
    let phys = match shm_alloc_frames(frames) {
        Some(p) => p,
        None => return Err(crate::abi::errno::ENOMEM),
    };
    let handle = NEXT_SHM_HANDLE.fetch_add(1, Ordering::Relaxed) as u64;
    let va = match shm_map_pages(handle, &phys) {
        Some(v) => v,
        None => {
            for p in &phys {
                pmm::free_frame(*p);
            }
            return Err(crate::abi::errno::ENOMEM);
        }
    };
    SHM_REGIONS.lock().push(Some(SharedRegion {
        handle,
        phys,
        refs: 1,
    }));
    crate::log::kdebug!("umem: shm {} frames at {:#x} (handle {:#x})", frames, va, handle);
    Ok(handle)
}

/// Map an existing shared region into the caller by handle. Returns the
/// virtual base (freshly reserved for this address space).
pub fn shm_map(handle: u64) -> Result<usize, i64> {
    if cur_id() == 0 {
        return Err(crate::abi::errno::EPERM);
    }
    let slot = shm_slot(handle).ok_or(crate::abi::errno::ENOENT)?;
    let phys = {
        let g = SHM_REGIONS.lock();
        g.get(slot).and_then(|s| s.as_ref()).ok_or(crate::abi::errno::ENOENT)?.phys.clone()
    };
    let va = shm_map_pages(handle, &phys).ok_or(crate::abi::errno::ENOMEM)?;
    {
        // A peer cannot be destroying the region mid-syscall: destruction only
        // runs from another process, which cannot execute until this syscall
        // returns, and the creator's own ref is still held until then.
        let mut g = SHM_REGIONS.lock();
        if let Some(r) = g.get_mut(slot).and_then(|s| s.as_mut()) {
            r.refs += 1;
        }
    }
    Ok(va)
}

/// Drop the caller's mapping(s) of `handle`. The frames are returned to the
/// allocator when the last mapping (across all processes) is gone.
pub fn shm_destroy(handle: u64) -> Result<(), i64> {
    if cur_id() == 0 {
        return Err(crate::abi::errno::EPERM);
    }
    if shm_slot(handle).is_none() {
        return Err(crate::abi::errno::ENOENT);
    }
    let n = shm_unmap_own(handle)?;
    shm_release_n(handle, n as u32);
    Ok(())
}

// ---------------------------------------------------------------------------
// Address-space cloning / teardown (fork, exec, exit)
// ---------------------------------------------------------------------------

/// Per-frame `va -> phys` map of every grant that aliases physical memory
/// shared across processes: device MMIO, DMA buffers and shared-memory
/// regions. Such pages are re-mapped (not deep-copied) on fork and never
/// freed inline on teardown (SHM frames go back to the allocator through their
/// region refcount instead).
fn active_shared_map() -> BTreeMap<usize, usize> {
    let mut out = BTreeMap::new();
    for (&va, m) in MAPPINGS.lock().iter() {
        if matches!(m.kind, GrantKind::Device | GrantKind::Dma | GrantKind::Shared) {
            for f in 0..m.frames {
                out.insert(va + f * FRAME_SIZE, m.phys_base + f * FRAME_SIZE);
            }
        }
    }
    out
}

/// Deep-copy `parent_root`'s user half into a fresh address space. Ordinary
/// pages (image, stack, anonymous mappings) are copied page-by-page; device
/// (MMIO), DMA and shared-memory grants are shared by re-mapping the same
/// physical frames. Returns the child's CR3 root.
pub fn clone_user_as(parent_root: usize) -> Result<usize, i64> {
    let shared = active_shared_map();
    // Which region each shared *frame* belongs to, so the child's new mapping
    // sets can be refcounted exactly (see `shm_retain`).
    let shm_owner = shm_phys_to_handle();
    let mut child = vmm::AddressSpace::new();
    child.clone_kernel_half(vmm::kernel_root());
    let parent = vmm::AddressSpace::from_root(parent_root);

    let mut ok = true;
    let mut retained: Vec<u64> = Vec::new();
    parent.for_each_user_leaf(&mut |va, phys, flags| {
        if !ok {
            return;
        }
        let flags = flags | vmm::PRESENT;
        if let Some(sp) = shared.get(&va) {
            // SAFETY: shared physical frame; freshly built child tables.
            unsafe { child.map_page(va, *sp, flags) };
            // One new mapping set per region the child inherits — retain once
            // per region, not once per page. Retaining here (rather than after
            // the walk) means a failed clone's teardown — `destroy_user_as`
            // releases exactly the shared leaves it walks — balances every
            // retain, so refcounts cannot drift.
            if let Some(h) = shm_owner.get(&phys) {
                if !retained.contains(h) {
                    retained.push(*h);
                    shm_retain(*h);
                }
            }
            return;
        }
        let np = match pmm::alloc_frame() {
            Some(f) => f,
            None => {
                ok = false;
                return;
            }
        };
        // SAFETY: both aliases are mapped in the kernel half; frame sizes
        // match, so the iterative copy is exact.
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(phys) as *const u8,
                phys_to_virt(np) as *mut u8,
                FRAME_SIZE,
            );
            child.map_page(va, np, flags);
        }
    });
    if !ok {
        // Abandon whatever was built: return the frames to the PMM. Shared
        // leaves were already refcounted above and are released again here,
        // which keeps every retain/release pair balanced.
        destroy_user_as(child.root());
        return Err(crate::abi::errno::ENOSYS);
    }
    Ok(child.root())
}

/// Tear down a user address space: free owned leaf frames (everything except
/// shared device/DMA grants) and every user-half page-table frame, including
/// the root PML4. The kernel half is shared and left untouched. Shared-memory
/// frames are returned through their region refcount: one release per distinct
/// region this address space mapped.
pub fn destroy_user_as(root: usize) {
    let shared: Vec<usize> = active_shared_map().values().copied().collect();
    let space = vmm::AddressSpace::from_root(root);
    let mut shared_leaf_vas: BTreeSet<usize> = BTreeSet::new();
    space.for_each_user_leaf(&mut |va, phys, _flags| {
        if shared.contains(&phys) {
            // Shared leaves are released through their region refcount below,
            // never freed inline. Remember their VAs so the mapping-set count
            // per region is exact.
            shared_leaf_vas.insert(va);
            return;
        }
        pmm::free_frame(phys);
    });
    space.for_each_user_table(&mut |table_phys| pmm::free_frame(table_phys));
    // Release exactly one mapping set per region per VA this address space
    // actually held — a process may legitimately hold several (a region's
    // `SHM_CREATE` internal mapping plus explicit `SHM_MAP`s).
    let mut sets: BTreeMap<u64, u32> = BTreeMap::new();
    for (&vb, m) in MAPPINGS.lock().iter() {
        if matches!(m.kind, GrantKind::Shared) && shared_leaf_vas.contains(&vb) {
            *sets.entry(m.handle).or_insert(0) += 1;
        }
    }
    for (h, n) in sets {
        shm_release_n(h, n);
    }
}