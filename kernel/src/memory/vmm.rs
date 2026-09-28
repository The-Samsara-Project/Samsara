// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Virtual memory manager: x86-64 four-level page tables.
//!
//! At boot the assembly stub maps the low GiB both identically and into the
//! higher half. [`init`] builds a *fresh* address space that clones only the
//! kernel (upper) PML4 entries, drops the identity map, and switches to it —
//! proving the in-kernel VMM end to end.

use super::pmm;
use crate::sync::{OnceCell, Spinlock};

/// Page table entry flag: present in memory.
pub const PRESENT: u64 = 1 << 0;
/// Page table entry flag: writable.
pub const WRITABLE: u64 = 1 << 1;
/// Page table entry flag: accessible from userspace (ring 3).
pub const USER_ACCESSIBLE: u64 = 1 << 2;
/// Page table entry flag: huge page (2 MiB / 1 GiB).
pub const HUGE_PAGE: u64 = 1 << 7;
/// Page table entry flag: no-execute (NX), requires EFER.NXE.
pub const NO_EXECUTE: u64 = 1 << 63;
/// Page table entry flag: disable caching for device memory.
pub const CACHE_DISABLE: u64 = 1 << 4;
/// Page table entry flag: use write-through caching for device memory.
///
/// Together with [`CACHE_DISABLE`] this selects the architecturally safe
/// uncached mapping on the firmware-default PAT configuration.
pub const WRITE_THROUGH: u64 = 1 << 3;

/// A single 4 KiB page table page.
#[repr(C, align(4096))]
#[derive(Clone, Copy)]
struct Table([u64; 512]);

impl Table {
    /// Zeroed table.
    fn zeroed() -> Self {
        Table([0; 512])
    }
}

/// A page-table hierarchy rooted at a physical CR3 value.
pub struct AddressSpace {
    root_phys: usize,
}

impl AddressSpace {
    /// Allocate an empty address space (root frame from the PMM).
    pub fn new() -> Self {
        let root_phys = pmm::alloc_frame().expect("pmm exhausted for PML4");
        let root = unsafe { &mut *(super::phys_to_virt(root_phys) as *mut Table) };
        *root = Table::zeroed();
        Self { root_phys }
    }

    /// Re-borrow an existing address space from its physical root.
    pub fn from_root(root_phys: usize) -> Self {
        Self { root_phys }
    }

    /// Clone every upper-half PML4 entry (kernel mappings) from `src`.
    pub fn clone_kernel_half(&mut self, src_root_phys: usize) {
        let src = unsafe { &*(super::phys_to_virt(src_root_phys) as *const Table) };
        let dst = unsafe { &mut *(super::phys_to_virt(self.root_phys) as *mut Table) };
        for i in 256..512 {
            dst.0[i] = src.0[i];
        }
    }

    /// Install this address space as the active one.
    pub fn switch_to(&self) {
        unsafe {
            core::arch::asm!(
                "mov cr3, {}",
                in(reg) self.root_phys,
                options(nomem, nostack)
            );
        }
    }

    /// Physical address of the root table (CR3 value).
    pub fn root(&self) -> usize {
        self.root_phys
    }

    fn table_at(entry: u64) -> &'static mut Table {
        let phys = (entry & 0x000F_FFFF_FFFF_F000) as usize;
        unsafe { &mut *(super::phys_to_virt(phys) as *mut Table) }
    }

    /// Map one 4 KiB page. Intermediate tables are allocated on demand.
    ///
    /// # Safety
    /// `virt` must not already be mapped, and must be a canonical address
    /// covered by this address space's layout contract.
    /// Point an already-present mapping at the same physical frame with
    /// different page-table flags.
    ///
    /// This is what `mprotect` needs. Unmapping and remapping would leave a
    /// window in which the virtual address is not present, and the caller holds
    /// no lock against another thread touching it -- so the leaf entry is
    /// rewritten in place instead.
    ///
    /// # Safety
    /// `virt` must already be mapped in this address space. The physical frame
    /// is not validated; the caller is responsible for it being one it owns.
    pub unsafe fn remap_page(&mut self, virt: usize, phys: usize, flags: u64) {
        debug_assert_eq!(virt & 0xFFF, 0);
        debug_assert_eq!(phys & 0xFFF, 0);

        let pt = match self.leaf_mut(virt) {
            // SAFETY: CPL 0, and the address space is the caller's own.
            Some(pt) => pt,
            // Not mapped. There is nothing to protect, and inventing a mapping
            // here would turn a caller's bug into a silent allocation. The
            // caller treats `None` as "this address is not yours".
            None => return,
        };
        let index = (virt >> 12) & 0x1FF;
        let entry = (phys as u64 & 0x000F_FFFF_FFFF_F000)
            | PRESENT
            | (if flags & WRITABLE != 0 { WRITABLE } else { 0 })
            | (if flags & USER_ACCESSIBLE != 0 { USER_ACCESSIBLE } else { 0 })
            | (if flags & NO_EXECUTE != 0 { NO_EXECUTE } else { 0 });
        pt.0[index] = entry;
    }

    /// The page table holding the leaf entry for `virt`, if the mapping exists.
    ///
    /// # Safety
    /// Caller must be at CPL 0 and the address space must be live.
    unsafe fn leaf_mut(&mut self, virt: usize) -> Option<&mut Table> {
        let pml4_index = (virt >> 39) & 0x1FF;
        let pdpt_index = (virt >> 30) & 0x1FF;
        let pd_index = (virt >> 21) & 0x1FF;

        let pml4 = &mut *(super::phys_to_virt(self.root_phys) as *mut Table);
        if pml4.0[pml4_index] & PRESENT == 0 {
            return None;
        }
        let pdpt = &mut *(super::phys_to_virt((pml4.0[pml4_index] & 0x000F_FFFF_FFFF_F000) as usize) as *mut Table);
        if pdpt.0[pdpt_index] & PRESENT == 0 {
            return None;
        }
        let pd = &mut *(super::phys_to_virt((pdpt.0[pdpt_index] & 0x000F_FFFF_FFFF_F000) as usize) as *mut Table);
        if pd.0[pd_index] & PRESENT == 0 {
            return None;
        }
        let pt_phys = (pd.0[pd_index] & 0x000F_FFFF_FFFF_F000) as usize;
        if pt_phys == 0 {
            return None;
        }
        Some(&mut *(super::phys_to_virt(pt_phys) as *mut Table))
    }

    /// Map one physical frame at `virt` with the given page-table flags.
    ///
    /// # Safety
    /// `virt` and `phys` must be 4 KiB aligned, and the caller must own `phys`.
    pub unsafe fn map_page(&mut self, virt: usize, phys: usize, flags: u64) {
        debug_assert_eq!(virt & 0xFFF, 0);
        debug_assert_eq!(phys & 0xFFF, 0);

        let pml4_index = (virt >> 39) & 0x1FF;
        let pdpt_index = (virt >> 30) & 0x1FF;
        let pd_index = (virt >> 21) & 0x1FF;
        let pt_index = (virt >> 12) & 0x1FF;

        let pml4 = &mut *(super::phys_to_virt(self.root_phys) as *mut Table);
        // Intermediate descriptors must carry the user bit too, or user-mode
        // access to any page beneath them is a protection violation. The U/S
        // bit is required at every level for a user mapping.
        let mid = PRESENT | WRITABLE | if flags & USER_ACCESSIBLE != 0 { USER_ACCESSIBLE } else { 0 };
        if pml4.0[pml4_index] & PRESENT == 0 {
            let f = pmm::alloc_frame().expect("pmm exhausted for PDPT");
            *Self::table_at_mut(f) = Table::zeroed();
            pml4.0[pml4_index] = (f as u64) | mid;
        }
        let pdpt = Self::table_at(pml4.0[pml4_index]);
        if pdpt.0[pdpt_index] & PRESENT == 0 {
            let f = pmm::alloc_frame().expect("pmm exhausted for PD");
            *Self::table_at_mut(f) = Table::zeroed();
            pdpt.0[pdpt_index] = (f as u64) | mid;
        }
        let pd = Self::table_at(pdpt.0[pdpt_index]);
        if pd.0[pd_index] & PRESENT == 0 {
            let f = pmm::alloc_frame().expect("pmm exhausted for PT");
            *Self::table_at_mut(f) = Table::zeroed();
            pd.0[pd_index] = (f as u64) | mid;
        }
        let pt = Self::table_at(pd.0[pd_index]);
        assert_eq!(pt.0[pt_index] & PRESENT, 0, "remap of {:#x}", virt);
        // The leaf entry reflects the caller's flags: NX is honoured only if
        // the caller sets it, so user code pages can be executable. The
        // intermediate descriptors above intentionally omit NX so they do not
        // blanket-disable execution for the subtree.
        pt.0[pt_index] = (phys as u64) | flags | PRESENT;
    }

    fn table_at_mut(phys: usize) -> &'static mut Table {
        unsafe { &mut *(super::phys_to_virt(phys) as *mut Table) }
    }

    /// Remove the 4 KiB mapping at `virt`, returning its backing physical
    /// frame. Intermediate tables remain allocated.
    ///
    /// # Safety
    /// Caller must ensure the address space is not concurrently active on
    /// another CPU (single-CPU kernel; the caller flushes the TLB as needed).
    pub unsafe fn unmap_page(&mut self, virt: usize) -> Option<usize> {
        let pml4_index = (virt >> 39) & 0x1FF;
        let pdpt_index = (virt >> 30) & 0x1FF;
        let pd_index = (virt >> 21) & 0x1FF;
        let pt_index = (virt >> 12) & 0x1FF;

        let pml4 = unsafe { &mut *(super::phys_to_virt(self.root_phys) as *mut Table) };
        let pdpt = if pml4.0[pml4_index] & PRESENT != 0 {
            Some(Self::table_at(pml4.0[pml4_index]))
        } else {
            None
        };
        let pd = match pdpt {
            Some(p) if p.0[pdpt_index] & PRESENT != 0 => Some(Self::table_at(p.0[pdpt_index])),
            _ => None,
        };
        let pt = match pd {
            Some(p) if p.0[pd_index] & PRESENT != 0 => Some(Self::table_at(p.0[pd_index])),
            _ => None,
        };
        match pt {
            Some(p) if p.0[pt_index] & PRESENT != 0 => {
                let phys = (p.0[pt_index] & 0xF_FFFF_FFFF_F000) as usize;
                p.0[pt_index] = 0;
                Some(phys)
            }
            _ => None,
        }
    }

    /// Map a physically contiguous range of `frames` starting at `phys` into
    /// `[virt, virt + frames*FRAME_SIZE)` with `flags`.
    ///
    /// # Safety
    /// Caller guarantees the destination range is unmapped and the physical
    /// frames are valid to expose at the requested permission level.
    pub unsafe fn map_contiguous(
        &mut self,
        virt: usize,
        phys: usize,
        frames: usize,
        flags: u64,
    ) {
        for i in 0..frames {
            self.map_page(virt + i * pmm::FRAME_SIZE, phys + i * pmm::FRAME_SIZE, flags);
        }
    }

    /// Translate a virtual address to `(physical, flags)` if mapped.
    pub fn translate(&self, virt: usize) -> Option<(usize, u64)> {
        let pml4 = unsafe { &*(super::phys_to_virt(self.root_phys) as *const Table) };
        let e = pml4.0[(virt >> 39) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        let pdpt = Self::table_at(e);
        let e = pdpt.0[(virt >> 30) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        if e & HUGE_PAGE != 0 {
            return Some(((e as usize & 0x3F_FFFF_C000_0000) | (virt & 0x3FFF_FFFF), e));
        }
        let pd = Self::table_at(e);
        let e = pd.0[(virt >> 21) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        if e & HUGE_PAGE != 0 {
            return Some(((e as usize & 0xFFFF_FFFF_E000_0000) | (virt & 0x1F_FFFF), e));
        }
        let pt = Self::table_at(e);
        let e = pt.0[(virt >> 12) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        Some(((e as usize & 0xF_FFFF_FFFF_F000) | (virt & 0xFFF), e))
    }

    /// Return the leaf (PT) entry for `virt`, if a 4 KiB mapping exists.
    pub fn translate_pt(&self, virt: usize) -> Option<u64> {
        let pml4 = unsafe { &*(super::phys_to_virt(self.root_phys) as *const Table) };
        let e = pml4.0[(virt >> 39) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        let pdpt = Self::table_at(e);
        let e = pdpt.0[(virt >> 30) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        let pd = Self::table_at(e);
        let e = pd.0[(virt >> 21) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        Some(e)
    }

    /// Return the (PD) entry for `virt`, if present.
    pub fn translate_pd(&self, virt: usize) -> Option<u64> {
        let pml4 = unsafe { &*(super::phys_to_virt(self.root_phys) as *const Table) };
        let e = pml4.0[(virt >> 39) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        let pdpt = Self::table_at(e);
        let e = pdpt.0[(virt >> 30) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        Some(e)
    }

    /// Return the (PDPT) entry for `virt`, if present.
    pub fn translate_pdpt(&self, virt: usize) -> Option<u64> {
        let pml4 = unsafe { &*(super::phys_to_virt(self.root_phys) as *const Table) };
        let e = pml4.0[(virt >> 39) & 0x1FF];
        if e & PRESENT == 0 {
            return None;
        }
        Some(e)
    }

    /// Visit every present 4 KiB leaf in the *user* half (PML4 indices 0..256)
    /// of this address space. Huge pages are split into 4 KiB leaves and
    /// reported with the huge-page flag cleared. Called with `(virt, phys, flags)`.
    ///
    /// The kernel half is never walked: it is shared with the kernel AS by
    /// clone, so forking / destroying a user space must not touch it.
    pub fn for_each_user_leaf(&self, f: &mut dyn FnMut(usize, usize, u64)) {
        let pml4 = unsafe { &*(super::phys_to_virt(self.root_phys) as *const Table) };
        for (i, &pml4e) in pml4.0[..256].iter().enumerate() {
            if pml4e & PRESENT == 0 {
                continue;
            }
            let pdpt = Self::table_at(pml4e);
            for (j, &e2) in pdpt.0.iter().enumerate() {
                if e2 & PRESENT == 0 {
                    continue;
                }
                let gb_base = (i << 39) | (j << 30);
                if e2 & HUGE_PAGE != 0 {
                    // 1 GiB page: split into 4 KiB frames.
                    let phys_base = (e2 as usize) & 0x000F_FFFF_C000_0000;
                    let flags = e2 & (0xFFF | (1 << 63));
                    for k in 0..(1 << 18) {
                        f(gb_base + k * pmm::FRAME_SIZE, phys_base + k * pmm::FRAME_SIZE, flags);
                    }
                    continue;
                }
                let pd = Self::table_at(e2);
                for (k, &e3) in pd.0.iter().enumerate() {
                    if e3 & PRESENT == 0 {
                        continue;
                    }
                    let mb_base = gb_base | (k << 21);
                    if e3 & HUGE_PAGE != 0 {
                        // 2 MiB page: split into 4 KiB frames.
                        let phys_base = (e3 as usize) & 0x000F_FFFF_FFE0_0000;
                        let flags = e3 & (0xFFF | (1 << 63));
                        for m in 0..512 {
                            f(
                                mb_base + m * pmm::FRAME_SIZE,
                                phys_base + m * pmm::FRAME_SIZE,
                                flags,
                            );
                        }
                        continue;
                    }
                    let pt = Self::table_at(e3);
                    for (m, &e4) in pt.0.iter().enumerate() {
                        if e4 & PRESENT == 0 {
                            continue;
                        }
                        f(
                            mb_base + m * pmm::FRAME_SIZE,
                            (e4 as usize) & 0x000F_FFFF_FFFF_F000,
                            e4 & (0xFFF | (1 << 63)),
                        );
                    }
                }
            }
        }
    }

    /// Visit the physical frame of every page-table page backing the *user*
    /// half (PDPT, PD, and PT tables plus the root PML4 last). Used to return
    /// a forked/destroyed address space to the PMM; the shared kernel-half
    /// tables are deliberately excluded.
    pub fn for_each_user_table(&self, f: &mut dyn FnMut(usize)) {
        let pml4 = unsafe { &*(super::phys_to_virt(self.root_phys) as *const Table) };
        for (i, &pml4e) in pml4.0[..256].iter().enumerate() {
            if pml4e & PRESENT == 0 {
                continue;
            }
            let pdpt_phys = (pml4e as usize) & 0x000F_FFFF_FFFF_F000;
            let pdpt = Self::table_at(pml4e);
            for &e2 in pdpt.0.iter() {
                if e2 & PRESENT == 0 {
                    continue;
                }
                if e2 & HUGE_PAGE != 0 {
                    continue; // 1 GiB leaf, no shadow table
                }
                let pd_phys = (e2 as usize) & 0x000F_FFFF_FFFF_F000;
                let pd = Self::table_at(e2);
                for &e3 in pd.0.iter() {
                    if e3 & PRESENT == 0 {
                        continue;
                    }
                    if e3 & HUGE_PAGE != 0 {
                        continue; // 2 MiB leaf, no shadow table
                    }
                    f((e3 as usize) & 0x000F_FFFF_FFFF_F000);
                }
                f(pd_phys);
            }
            f(pdpt_phys);
        }
        f(self.root_phys);
    }
}

static ACTIVE_SPACE: OnceCell<Spinlock<AddressSpace>> = OnceCell::new();

/// Read the current CR3 value.
fn current_cr3() -> usize {
    let v: usize;
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) v, options(nomem, nostack));
    }
    v
}

/// Enable EFER.NXE so the NO_EXECUTE page flag is legal.
fn enable_nxe_bit() {
    const MSR_EFER: u32 = 0xC000_0080;
    const EFER_NXE: u64 = 1 << 11;
    unsafe {
        let lo: u32;
        let hi: u32;
        core::arch::asm!(
            "rdmsr",
            in("ecx") MSR_EFER,
            out("eax") lo,
            out("edx") hi,
            options(nostack)
        );
        let value = (((hi as u64) << 32) | lo as u64) | EFER_NXE;
        core::arch::asm!(
            "wrmsr",
            in("ecx") MSR_EFER,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nostack)
        );
    }
}

/// Build the permanent kernel address space and activate it.
///
/// The identity mapping installed by the boot stub is intentionally dropped:
/// only the higher-half kernel/physical-map entries survive.
pub fn init() {
    enable_nxe_bit();

    let boot_cr3 = current_cr3();

    let mut space = AddressSpace::new();
    space.clone_kernel_half(boot_cr3);

    crate::log::kdebug!(
        "vmm: switching cr3 {:#x} -> {:#x} (identity map removed)",
        boot_cr3,
        space.root()
    );

    if ACTIVE_SPACE.set(Spinlock::new(space)).is_err() {
        panic!("vmm initialized twice");
    }
    ACTIVE_SPACE.get().unwrap().lock().switch_to();
}

/// Map a contiguous virtual range with freshly allocated physical frames.
///
/// Returns once every page in `[virt, virt + size)` is backed by memory.
pub fn map_range(virt: usize, size: usize, flags: u64) {
    let mut guard = match ACTIVE_SPACE.get() {
        Some(l) => l.lock(),
        None => panic!("vmm used before initialization"),
    };
    let pages = size.div_ceil(pmm::FRAME_SIZE);
    for i in 0..pages {
        let frame = pmm::alloc_frame().expect("pmm exhausted while mapping range");
        // SAFETY: range is reserved exclusively by the caller.
        unsafe {
            guard.map_page(virt + i * pmm::FRAME_SIZE, frame, flags);
        }
    }
}

/// Map an existing physically contiguous range into the active address space.
///
/// No frames are allocated: the target pages already exist (device memory,
/// such as a bootloader framebuffer). `phys` must be page-aligned.
pub fn map_physical(virt: usize, phys: usize, frames: usize, flags: u64) {
    let mut guard = match ACTIVE_SPACE.get() {
        Some(l) => l.lock(),
        None => panic!("vmm used before initialization"),
    };
    // SAFETY: the caller guarantees the range is unmapped and the physical
    // pages are valid to expose at the requested permission level.
    unsafe {
        guard.map_contiguous(virt, phys, frames, flags);
    }
}

/// Look up the backing physical address of `virt`, if mapped.
pub fn translate(virt: usize) -> Option<(usize, u64)> {
    ACTIVE_SPACE.get()?.lock().translate(virt)
}

/// Translate `virt` through the page tables currently loaded in CR3.
///
/// Syscall handlers execute with the *calling process's* address space active
/// — the kernel half is shared through cloning, but a user-supplied buffer
/// only exists in that process's own lower half. Validating it against the
/// kernel's static [`ACTIVE_SPACE`] walks the wrong tables and rejects every
/// user pointer. Interrupts are masked for the whole syscall (SFMASK clears
/// IF), so the root cannot change between validation and use.
pub fn translate_current(virt: usize) -> Option<(usize, u64)> {
    AddressSpace::from_root(current_cr3()).translate(virt)
}

/// Physical root (CR3 value) of the kernel's own address space.
pub fn kernel_root() -> usize {
    ACTIVE_SPACE.get().expect("vmm not initialized").lock().root()
}

/// Invalidate the TLB entry for one 4 KiB page (single-CPU flush helper).
pub fn invlpg(va: usize) {
    unsafe {
        core::arch::asm!("invlpg [{0}]", in(reg) va, options(nomem, nostack));
    }
}
