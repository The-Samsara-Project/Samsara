// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Memory management subsystem.
//!
//! Layout of the Samsara virtual address space:
//!
//! ```text
//! 0x0000_0000_0000_0000 .. 0x0000_007F_FFFF_FFFF   userspace (reserved)
//! 0xFFFF_FF00_0000_0000                            kernel heap
//! 0xFFFF_FFFF_8000_0000 ..                         physical map (low GiB) +
//!                                                  higher-half kernel image
//! ```
//!
//! The current physical map covers the first GiB of memory; extending it to
//! all of physical memory is planned alongside SMP support.

pub mod heap;
pub mod pmm;
pub mod user_map;
pub mod vmm;

/// DMA allocation functions for device drivers
pub use pmm::{alloc_dma_pages, free_dma_pages};

use crate::multiboot2::BootInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Offset added to physical addresses to reach them through the physical map.
pub const PHYSMAP_BASE: usize = 0xFFFF_FFFF_8000_0000;

/// Base virtual address of the kernel heap.
pub const HEAP_VIRT_BASE: usize = 0xFFFF_FF00_0000_0000;

/// Initial size of the kernel heap.
pub const HEAP_SIZE: usize = 64 * 1024 * 1024;

/// Base virtual address of the device MMIO window allocator.
///
/// Sits immediately above the end of the heap (`HEAP_VIRT_BASE + HEAP_SIZE`),
/// in the otherwise-unmapped gap between the heap and the physical map. MMIO
/// regions (LAPIC, IOAPIC, HPET, ...) are mapped here with uncached semantics
/// so they ride along with the kernel half into every address space.
pub const DEVICE_MMIO_VIRT_BASE: usize = HEAP_VIRT_BASE + HEAP_SIZE;

/// Next free virtual page inside [`DEVICE_MMIO_VIRT_BASE`].
static DEVICE_MMIO_NEXT: AtomicUsize = AtomicUsize::new(DEVICE_MMIO_VIRT_BASE);

/// Map a device MMIO region at `phys` (page-aligned) into the kernel address
/// space with uncached semantics, returning its virtual address.
///
/// The window grows upward from [`DEVICE_MMIO_VIRT_BASE`]; because the mapping
/// lives in the kernel half it is inherited by every address space cloned off
/// the kernel root, so interrupt handlers can touch the device from arbitrary
/// CR3 contexts (i.e. while a user task is active).
pub fn map_device_mmio(phys: usize, size: usize) -> usize {
    let base_page = phys & !(pmm::FRAME_SIZE - 1);
    let offset = phys - base_page;
    let pages = (offset + size).div_ceil(pmm::FRAME_SIZE);
    let virt = DEVICE_MMIO_NEXT.fetch_add(pages * pmm::FRAME_SIZE, Ordering::SeqCst);
    vmm::map_physical(
        virt,
        base_page,
        pages,
        vmm::PRESENT | vmm::WRITABLE | vmm::NO_EXECUTE | vmm::CACHE_DISABLE | vmm::WRITE_THROUGH,
    );
    virt + offset
}

/// Highest physical address managed by the allocator.
const MAX_MANAGED_PHYS: u64 = 0x4000_0000; // 1 GiB

/// Classification of a firmware memory region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// Free for allocation by the kernel.
    Usable,
    /// In use by firmware / hardware / loader; never allocate.
    Reserved,
}

/// One contiguous physical memory region.
#[derive(Debug, Clone, Copy)]
pub struct MemRegion {
    /// Start physical address.
    pub start: u64,
    /// Size in bytes.
    pub size: u64,
    /// How the region may be used.
    pub kind: RegionKind,
}

impl MemRegion {
    /// End physical address (exclusive).
    pub fn end(&self) -> u64 {
        self.start + self.size
    }
}

impl Default for MemRegion {
    fn default() -> Self {
        Self {
            start: 0,
            size: 0,
            kind: RegionKind::Reserved,
        }
    }
}

/// Translate a physical address to its higher-half virtual alias.
#[inline(always)]
pub fn phys_to_virt(phys: usize) -> usize {
    phys.wrapping_add(PHYSMAP_BASE)
}

/// Translate a kernel virtual address belonging to the physical map back to
/// its physical address. Addresses outside the map return an arbitrary value;
/// callers must only pass addresses produced by [`phys_to_virt`].
#[inline(always)]
pub fn virt_to_phys(virt: usize) -> usize {
    virt.wrapping_sub(PHYSMAP_BASE)
}

/// Kernel image bounds in *physical* memory (from the linker script).
extern "C" {
    static __kernel_lma_start: u8;
    static __kernel_lma_end: u8;
}

/// Physical start of the loaded kernel image.
pub fn kernel_phys_start() -> usize {
    unsafe { &__kernel_lma_start as *const u8 as usize }
}

/// Physical end of the loaded kernel image.
pub fn kernel_phys_end() -> usize {
    unsafe { &__kernel_lma_end as *const u8 as usize }
}

/// Bring up the whole memory stack from the firmware-provided boot info.
pub fn init(boot_info: &BootInfo) {
    // 1. Physical memory: bitmap frame allocator over usable regions.
    pmm::init(
        &boot_info.regions[..boot_info.region_count],
        kernel_phys_start(),
        kernel_phys_end(),
    );

    // 2. Virtual memory: fresh page tables, identity map removed.
    vmm::init();

    // 3. Kernel heap on top of the VMM + PMM.
    heap::init(HEAP_VIRT_BASE, HEAP_SIZE);

    crate::log::kinfo!(
        "memory: pmm + vmm + {} MiB heap initialized",
        HEAP_SIZE >> 20
    );
}
