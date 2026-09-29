// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! In-kernel ELF loader and dynamic linker.
//!
//! Ring-3 servers are built as static-position-independent ELF64 binaries
//! (`-pie`, `ET_DYN`): the kernel maps their `PT_LOAD` segments with
//! per-segment permissions and then plays the role of ld.so — reading the
//! dynamic table, walking `.rela.dyn`/`.rela.plt` and resolving every
//! relocation before the process is permitted to run (an eager, `BIND_NOW`
//! style load). PIE images are mapped at their link-time virtual addresses
//! (load bias zero), exactly as the Linux kernel handles static-PIE.
//!
//! The image itself is embedded kernel memory, so table reads come straight
//! from those bytes via "vaddr -> file offset" lowering through the segments.
//! Relocation *targets* are written through the freshly built page tables via
//! physical aliases, which keeps the loader independent of whatever address
//! space the CPU currently has active.
//!
//! A SysV initial stack (`argc`/`argv`/`envp` + auxv) is staged at the top of
//! the user stack so a future userspace bootstrap can read `AT_PHDR`,
//! `AT_PAGESZ` and `AT_ENTRY`, matching a real process image.

use alloc::string::String;
use alloc::vec::Vec;
use crate::abi::errno::{ENOEXEC, ENOMEM};
use crate::memory::pmm;
use crate::memory::phys_to_virt;
use crate::memory::vmm;

/// Frame / page size assumed by the loader (matches `pmm::FRAME_SIZE`).
pub const PAGE: usize = 4096;
/// Upper bound for user image addresses: segments must stay below the
/// anonymous grant cursor (see `user_map::NEXT_ANON_VA`, 4 GiB).
const MAX_IMAGE_END: usize = 0x0000_0040_0000_0000;

// e_ident offsets.
const EI_CLASS: usize = 4;
const EI_DATA: usize = 5;
const EI_VERSION: usize = 6;
const ELFCLASS64: u8 = 2;
const ELFDATA2LSB: u8 = 1;
const EV_CURRENT: u8 = 1;

// e_type / e_machine.
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;
const EM_X86_64: u16 = 62;

// Program header types / flags.
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_TLS: u32 = 7;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const PF_R: u32 = 4;

// Dynamic table tags.
const DT_NULL: i64 = 0;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const DT_RELAENT: i64 = 9;
const DT_SYMENT: i64 = 11;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;

// x86-64 dynamic relocation types found in -pie images.
const R_X86_64_NONE: u64 = 0;
const R_X86_64_64: u64 = 1;
const R_X86_64_GLOB_DAT: u64 = 6;
const R_X86_64_JUMP_SLOT: u64 = 7;
const R_X86_64_RELATIVE: u64 = 8;

// auxv tags written to the initial stack.
const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;
/// Address of the thread-control block the loader built for this image, or 0 for
/// an image with no `PT_TLS`.
///
/// A Samsara extension, and deliberately above Linux's own auxv range (which tops
/// out in the low tens) so it cannot collide with a tag added upstream later.
///
/// It exists because of a bootstrap cycle. A libc's thread-control block is
/// normally found by reading the thread pointer and treating it as the block's
/// address -- but the first field *in* that block is a self-pointer, which has to
/// already hold the block's address for the read to return anything useful. mlibc
/// breaks the cycle inside its dynamic loader, which allocates the block and
/// therefore knows its address. There is no dynamic loader here: the kernel
/// replaced it, so the kernel is the one that knows, and it has to say so
/// somewhere. The initial stack's auxv is where a loader says things like this,
/// and the port reads it before it touches any thread-local variable.
///
/// The kernel does *not* also write the self-pointer itself. Filling in a field
/// of somebody else's C++ object is the port's job, and the offset of that field
/// is mlibc's business, not the loader's.
const AT_TCB: u64 = 0x100;

/// Upper bounds on the pointer arrays built for the initial stack. These cap
/// how much of a pathological argv/env the loader will stage; a program that
/// passes more is truncated rather than allowed to run the stack down.
const MAX_ARG: usize = 64;
const MAX_ENV: usize = 64;

/// Bytes reserved for the thread-control block that the libc constructs in
/// place, on top of the image's own thread-local storage.
///
/// The block is mlibc's `Tcb`, which is a C++ type in a library the kernel does
/// not link, so the kernel cannot measure it and only reserves room. The
/// reservation is not a guess, though: the port static-asserts at compile time
/// that `sizeof(Tcb)` fits inside it, so an mlibc whose `Tcb` outgrew the
/// reservation would fail to build the port rather than have the kernel
/// scribble past the end of a thread block. 152 bytes is `sizeof(Tcb)` for this
/// build; 256 leaves headroom without costing a page.
const TCB_RESERVE: usize = 256;

/// The alignment `Tcb` itself requires. Part of the thread block's layout for
/// the same reason as [`TCB_RESERVE`]: the type belongs to the libc, and this is
/// the architecture's own requirement for an eight-byte-aligned object.
const TCB_ALIGN: usize = 8;

/// One parsed `PT_TLS`: the image's thread-local storage template.
///
/// `file_sz` bytes of initialized data live in the image at `vaddr`; the block
/// is `mem_sz` bytes in total, so the tail (`mem_sz - file_sz`) is zero
/// `.tbss`. The image's copy is a *template* -- the loader relocates it into the
/// thread block rather than running the program against it, because the
/// thread-control block has to sit immediately above the thread-local data and
/// the image's `.tbss` is followed by the program's own `.bss`.
#[derive(Clone, Copy)]
struct Tls {
    vaddr: usize,
    file_sz: usize,
    mem_sz: usize,
    align: usize,
}

/// One parsed `PT_LOAD` (or the synthetic lead-in that maps the ELF headers).
#[derive(Clone, Copy)]
struct Seg {
    file_off: usize,
    vaddr: usize,
    file_sz: usize,
    mem_sz: usize,
    flags: u32,
}

impl Seg {
    fn perms(&self) -> u64 {
        let mut f = vmm::USER_ACCESSIBLE;
        if self.flags & PF_W != 0 {
            f |= vmm::WRITABLE;
        }
        if self.flags & PF_X == 0 {
            f |= vmm::NO_EXECUTE;
        }
        f
    }
}

/// Little-endian field readers over an image slice.
#[inline]
fn u16_at(b: &[u8], o: usize) -> Option<u16> {
    b.get(o..o + 2).map(|s| u16::from_le_bytes([s[0], s[1]]))
}
#[inline]
fn u32_at(b: &[u8], o: usize) -> Option<u32> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
#[inline]
fn u64_at(b: &[u8], o: usize) -> Option<u64> {
    b.get(o..o + 8).map(|s| {
        u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]])
    })
}

/// Parsed ELF64 header.
struct Header {
    entry: usize,
    phoff: usize,
    phentsize: usize,
    phnum: usize,
}

/// Validate the ELF64 header: magic, class, endianness, machine, structure
/// sizes — and accept the kinds of files we can load.
fn parse_header(img: &[u8]) -> Option<Header> {
    if img.len() < 64 || &img[0..4] != b"\x7fELF" {
        return None;
    }
    if img[EI_CLASS] != ELFCLASS64 || img[EI_DATA] != ELFDATA2LSB || img[EI_VERSION] != EV_CURRENT
    {
        return None;
    }
    let e_type = u16_at(img, 16)?;
    if e_type != ET_DYN && e_type != ET_EXEC {
        return None;
    }
    if u16_at(img, 18)? != EM_X86_64 {
        return None;
    }
    let phentsize = u16_at(img, 54)? as usize;
    let phnum = u16_at(img, 56)? as usize;
    if phentsize < 56 || phnum == 0 || phnum > 128 {
        return None;
    }
    let phoff = u64_at(img, 32)? as usize;
    if phoff.checked_add(phentsize * phnum).map_or(true, |e| e > img.len()) {
        return None;
    }
    Some(Header { entry: u64_at(img, 24)? as usize, phoff, phentsize, phnum })
}

/// Program headers, split into loadable segments plus the file offset of the
/// `PT_DYNAMIC` table (when the linker emitted one).
struct Phdrs {
    segs: Vec<Seg>,
    dynamic_file: Option<usize>,
    tls: Option<Tls>,
}

fn parse_phdrs(img: &[u8], h: &Header) -> Option<Phdrs> {
    let mut segs = Vec::new();
    let mut dynamic_file = None;
    let mut tls = None;
    for i in 0..h.phnum {
        let p = h.phoff + i * h.phentsize;
        let p_type = u32_at(img, p)?;
        let flags = u32_at(img, p + 4)?;
        let off = u64_at(img, p + 8)?;
        let vaddr = u64_at(img, p + 16)?;
        let file_sz = u64_at(img, p + 32)?;
        let mem_sz = u64_at(img, p + 40)?;
        let align = u64_at(img, p + 48)?;
        match p_type {
            PT_DYNAMIC => {
                if file_sz != 0 {
                    dynamic_file = Some(off as usize);
                }
            }
            PT_TLS => {
                // `p_align` is a promise about `p_vaddr`, and an image is free to
                // make it 0 or 1 to say "unaligned". Nothing downstream needs
                // more than 8 for the block itself, and the tail is zero-filled
                // either way, so a smaller claim is simply raised to the
                // architecture's floor rather than rejected.
                let a = (align as usize).max(TCB_ALIGN);
                tls = Some(Tls {
                    vaddr: vaddr as usize,
                    file_sz: file_sz as usize,
                    mem_sz: mem_sz as usize,
                    align: a,
                });
            }
            PT_LOAD => {
                let (off, vaddr, file_sz, mem_sz) =
                    (off as usize, vaddr as usize, file_sz as usize, mem_sz as usize);
                if mem_sz == 0 || off.checked_add(file_sz).map_or(true, |e| e > img.len()) {
                    crate::log::kdebug!(
                        "elf: PT_LOAD {i} file span {off:#x}+{file_sz:#x} outside image {}",
                        img.len()
                    );
                    return None;
                }
                if vaddr.checked_add(mem_sz).map_or(true, |e| e > MAX_IMAGE_END) {
                    crate::log::kdebug!(
                        "elf: PT_LOAD {i} vaddr {vaddr:#x}+{mem_sz:#x} past MAX_IMAGE_END {MAX_IMAGE_END:#x}"
                    );
                    return None;
                }
                // A load starting at or below the first page is rejected: the
                // null page must never be mapped, and a segment that reaches
                // into it would put executable text at an address that
                // dereferencing NULL silently lands in. This is why images must
                // be linked above PAGE -- the same rule the kernel's own linker
                // script imposes.
                if vaddr < PAGE {
                    crate::log::kdebug!(
                        "elf: PT_LOAD {i} vaddr {vaddr:#x} is below the first page"
                    );
                    return None;
                }
                segs.push(Seg { file_off: off, vaddr, file_sz, mem_sz, flags });
            }
            _ => {}
        }
    }
    if segs.is_empty() {
        return None;
    }
    Some(Phdrs {
        segs,
        dynamic_file,
        tls,
    })
}

/// Map one loaded segment (with its `mem_sz - file_sz` zero fill) into
/// `aspace`. Content is copied from `img` through kernel-half aliases so no
/// particular CR3 has to be active.
fn map_segment(aspace: &mut vmm::AddressSpace, img: &[u8], s: &Seg) -> Result<(), i64> {
    let page_start = s.vaddr & !(PAGE - 1);
    let page_end = (s.vaddr + s.mem_sz + PAGE - 1) & !(PAGE - 1);
    let mut page = page_start;
    while page < page_end {
        let phys = match pmm::alloc_frame() {
            Some(p) => p,
            None => {
                // Named rather than a bare `ok_or(ENOMEM)`, because "out of memory"
                // with no indication of what was being mapped is the least useful
                // line in a boot log. The segment address and the page count
                // within it are what tell the two possible causes apart: an
                // allocator that was already nearly empty fails on the first
                // page of the first segment, and one that ran out part-way fails
                // deep into a segment that had been succeeding.
                let (used, total) = pmm::stats();
                crate::log::kwarn!(
                    "elf: no frame at vaddr {:#x} (segment {:#x}+{:#x}, {}/{} pages in), pmm {used}/{total}",
                    page,
                    s.vaddr,
                    s.mem_sz,
                    (page - page_start) / PAGE,
                    (page_end - page_start) / PAGE,
                );
                return Err(ENOMEM);
            }
        };
        // SAFETY: `page` is freshly reserved in this address space; the frame
        // is written through its global kernel alias before user exposure.
        unsafe { aspace.map_page(page, phys, s.perms()) };
        let dst = phys_to_virt(phys) as *mut u8;
        unsafe { core::ptr::write_bytes(dst, 0, page_end.min(page + PAGE) - page) };

        // Copy this page's share of file bytes; the zero fill covers the
        // .bss remainder of the segment.
        let lo = page.max(s.vaddr);
        let hi = (page + PAGE).min(s.vaddr + s.file_sz);
        if hi > lo {
            // SAFETY: `hi - lo` is within `s.file_sz` (validated), and the
            // destination page is our freshly allocated frame.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    img.as_ptr().add(s.file_off + (lo - s.vaddr)),
                    dst.add(lo - page),
                    hi - lo,
                );
            }
        }
        page += PAGE;
    }
    Ok(())
}

/// Map `vaddr` to the file offset holding that byte, or `None` if the address
/// is outside every segment's file-covered range.
fn file_off(segs: &[Seg], vaddr: usize) -> Option<usize> {
    segs.iter().find_map(|s| {
        (s.vaddr <= vaddr && vaddr < s.vaddr + s.file_sz).then(|| s.file_off + (vaddr - s.vaddr))
    })
}

/// Walk a dynamic table's `d_tag`/`d_val` pairs from `dyn_file` until `DT_NULL`,
/// returning `(rela_off, rela_sz, rela_ent, symtab_off, strtab_off, syment)`
/// with the table addresses already lowered to *file* offsets via `lower`.
fn parse_dynamic(
    img: &[u8],
    dyn_file: usize,
    lower: impl Fn(usize) -> Option<usize>,
) -> Result<(usize, usize, usize, usize, usize, usize), i64> {
    let mut rela = 0usize;
    let mut rela_sz = 0usize;
    let mut rela_ent = 24usize;
    let mut symtab = 0usize;
    let mut strtab = 0usize;
    let mut syment = 24usize;

    let mut off = dyn_file;
    let mut guard = 0usize;
    while off + 16 <= img.len() && guard < 256 {
        let tag = i64::from_le_bytes([
            img[off], img[off + 1], img[off + 2], img[off + 3],
            img[off + 4], img[off + 5], img[off + 6], img[off + 7],
        ]);
        let val = u64_at(img, off + 8).unwrap_or(0) as usize;
        match tag {
            DT_NULL => break,
            DT_RELA => rela = val,
            DT_RELASZ => rela_sz = val,
            DT_RELAENT => rela_ent = val.max(24),
            DT_SYMTAB => symtab = val,
            DT_STRTAB => strtab = val,
            DT_SYMENT => syment = val.max(24),
            _ => {}
        }
        guard += 1;
        off += 16;
    }
    // A fully linked image needs no relocations at all: every absolute address
    // is already in place, so an absent or empty DT_RELA/DT_RELASZ is normal and
    // not a malformed file. Rejecting it here made a correctly linked
    // static-PIE unloadable, which is exactly the shape a C program linked
    // against a sysroot takes.
    if rela_sz == 0 || rela == 0 {
        return Ok((0, 0, 24, 0, 0, 24));
    }
    if rela_ent < 24 {
        return Err(ENOEXEC);
    }
    // Lower DT virtual addresses to file offsets so we never dereference
    // user-space addresses from the kernel.
    let rela = lower(rela).ok_or(ENOEXEC)?;
    let symtab = lower(symtab).ok_or(ENOEXEC)?;
    let strtab = lower(strtab).ok_or(ENOEXEC)?;
    Ok((rela, rela_sz, rela_ent, symtab, strtab, syment))
}

/// Write a relocated pointer through the freshly built page tables.
fn write_addr(aspace: &vmm::AddressSpace, va: usize, val: u64) -> Result<(), i64> {
    let (phys, _) = aspace.translate(va).ok_or(ENOEXEC)?;
    // SAFETY: `phys` is a live frame of a page already mapped for this process.
    unsafe {
        (phys_to_virt(phys) as *mut u64).write_unaligned(val);
    }
    Ok(())
}

/// Apply every dynamic relocation in the RELA table. Images are loaded at
/// their link-time addresses (bias zero), so `R_X86_64_RELATIVE` stores
/// `base + A` and symbol-relative types store `base + S + A`. Returns the
/// number of relocations applied.
fn relocate(
    aspace: &vmm::AddressSpace,
    img: &[u8],
    segs: &[Seg],
    dyn_file: usize,
) -> Result<usize, i64> {
    let base: usize = 0; // static-PIE mapped at p_vaddr; AT_BASE = 0.
    let (rela, rela_sz, rela_ent, symtab, strtab, syment) =
        parse_dynamic(img, dyn_file, |v| file_off(segs, v))?;
    if rela_sz == 0 {
        // Nothing to relocate: every address in the image is already final.
        return Ok(0);
    }

    let n = rela_sz / rela_ent;
    let mut done = 0usize;
    for i in 0..n {
        let r = rela + i * rela_ent;
        if r + 24 > img.len() {
            return Err(ENOEXEC);
        }
        let target = u64_at(img, r).ok_or(ENOEXEC)? as usize;
        let info = u64_at(img, r + 8).ok_or(ENOEXEC)?;
        let addend = i64::from_le_bytes([
            img[r + 16], img[r + 17], img[r + 18], img[r + 19],
            img[r + 20], img[r + 21], img[r + 22], img[r + 23],
        ]);
        let sym = (info >> 32) as usize;
        let ty = info & 0xffff_ffff;

        // Relocation targets must land in mapped memory.
        if aspace.translate(target).is_none() {
            return Err(ENOEXEC);
        }

        let value: u64 = match ty {
            R_X86_64_NONE => continue,
            R_X86_64_RELATIVE => (base as u64).wrapping_add(addend as u64),
            R_X86_64_64 | R_X86_64_GLOB_DAT | R_X86_64_JUMP_SLOT => {
                // Read the referencing symbol's st_name, then scan the
                // dynsym sequentially for the definition (no interposition).
                let s = symtab
                    .checked_add(sym * syment)
                    .filter(|&x| x + syment <= img.len())
                    .ok_or(ENOEXEC)?;
                let st_name = u32_at(img, s).ok_or(ENOEXEC)? as usize;
                let nm = strtab
                    .checked_add(st_name)
                    .filter(|&x| x < img.len())
                    .ok_or(ENOEXEC)?;
                let mut name_end = nm;
                while name_end < img.len() && img[name_end] != 0 {
                    name_end += 1;
                }
                let name = &img[nm..name_end];

                let sym_val = (0..(img.len().saturating_sub(symtab)) / syment).find_map(|k| {
                    let ks = symtab + k * syment;
                    let kn = u32_at(img, ks).unwrap_or(0) as usize;
                    let km = strtab.checked_add(kn).unwrap_or(img.len());
                    (km < img.len() && img.get(km..km + name.len()) == Some(name))
                        .then(|| u64_at(img, ks + 8))
                        .flatten()
                });
                let sval = sym_val.ok_or(ENOEXEC)?;
                (base as u64).wrapping_add(sval).wrapping_add(addend as u64)
            }
            _ => return Err(ENOEXEC),
        };
        write_addr(aspace, target, value)?;
        done += 1;
    }
    Ok(done)
}

/// Stage the SysV initial stack at the top of the user stack, writing downward
/// so the initial `%rsp` points at `argc`:
///
/// ```text
///   [ string bytes for env then argv ]   <- highest addresses
///   [ 16-byte alignment padding ]
///   [ auxv pairs, terminated by AT_NULL ]
///   [ envp: p_env[0..n], NULL ]
///   [ argv: p_arg[0..n], NULL ]
///   [ argc ]
///   ^ rsp
/// ```
///
/// This is the layout a C runtime expects, so a program linked against a
/// libc finds its environment by walking the stack rather than by asking the
/// kernel. `args` and `env` are already `Vec<String>`; `env` may be empty, in
/// which case `envp` is just its NULL terminator.
fn write_entry_stack(
    aspace: &vmm::AddressSpace,
    stack_base: usize,
    stack_end: usize,
    phdr_vaddr: usize,
    phnum: usize,
    entry: usize,
    tcb: usize,
    args: &[String],
    env: &[String],
) -> usize {
    let auxv: [(u64, u64); 7] = [
        (AT_PHDR, phdr_vaddr as u64),
        (AT_PHENT, 56),
        (AT_PHNUM, phnum as u64),
        (AT_PAGESZ, PAGE as u64),
        (AT_BASE, 0),
        (AT_ENTRY, entry as u64),
        (AT_TCB, tcb as u64),
    ];
    // One byte at a time downward, so strings need no alignment care.
    let mut cur = stack_end;
    let put_bytes = |cur: &mut usize, bytes: &[u8], aspace: &vmm::AddressSpace| {
        for b in bytes.iter().rev() {
            if let Some((phys, _)) = aspace.translate(*cur - 1) {
                // SAFETY: `cur` lies inside the stack region mapped above.
                unsafe { (phys_to_virt(phys) as *mut u8).write(*b) };
            }
            *cur -= 1;
        }
    };
    let put_word = |cur: &mut usize, word: u64, aspace: &vmm::AddressSpace| {
        if let Some((phys, _)) = aspace.translate(*cur - 8) {
            // SAFETY: `cur` lies inside the stack region mapped above.
            unsafe { (phys_to_virt(phys) as *mut u64).write_unaligned(word) };
        }
        *cur -= 8;
    };

    // Strings first, at the very top. Each is written as terminator-then-body
    // so that a forward scan of the bytes finds the NUL (see below). `env` is
    // pushed before `args`, so the block reads env-then-arg in the same
    // descending order the pointer arrays are written in.
    let mut env_ptrs = [0u64; MAX_ENV];
    let mut arg_ptrs = [0u64; MAX_ARG];
    let mut n_env = 0usize;
    let mut n_arg = 0usize;

    // NUL terminator first, then the body.
    //
    // Order matters: `put_bytes` walks downward, so writing the terminator
    // first leaves it at the *higher* address, immediately after the string.
    // That is the only arrangement in which a left-to-right scan of the bytes
    // terminates. Writing the body first puts the NUL below the string, where
    // nothing ever reads it -- a `getenv`-style walk then runs off the end of
    // the string and, eventually, off the top of the stack.
    //
    // The first terminator lands at `stack_end - 1`, the last mapped byte,
    // which is why the cursor is left at `stack_end` rather than advanced past
    // it: `stack_end` itself is not part of the stack mapping.
    for e in env.iter().take(MAX_ENV) {
        put_bytes(&mut cur, &[0], aspace);
        put_bytes(&mut cur, e.as_bytes(), aspace);
        env_ptrs[n_env] = cur as u64;
        n_env += 1;
    }
    for a in args.iter().take(MAX_ARG) {
        put_bytes(&mut cur, &[0], aspace);
        put_bytes(&mut cur, a.as_bytes(), aspace);
        arg_ptrs[n_arg] = cur as u64;
        n_arg += 1;
    }

    // Everything below the strings is 8-byte words: the auxv pairs, both
    // pointer arrays with their terminators, and `argc`. The SysV ABI wants
    // `%rsp` 16-byte aligned at process entry, and `argc` is the *last* word
    // written, so the alignment has to be arranged backwards from it: an odd
    // number of words walked down from a 16-aligned cursor lands 8 mod 16.
    // Pre-positioning the cursor by that one extra word is what keeps `%rsp`
    // aligned for every argv length, not just the even-word ones.
    let words = 2 * (auxv.len() + 1) + (1 + n_env) + (1 + n_arg) + 1;
    cur = (cur - words * 8) & !15;
    if words % 2 == 1 {
        cur += 8;
    }

    // AT_NULL goes highest; auxv pairs follow in reverse so the first pair a
    // reader sees (after envp) is AT_PHDR. Each pair is laid out as tag at
    // the lower address, value above it.
    for (t, v) in core::iter::once((AT_NULL, 0u64)).chain(auxv.iter().rev().copied()) {
        for word in [v, t] {
            put_word(&mut cur, word, aspace);
        }
    }
    // envp: the NULL terminator sits at the *high* end of the block, above the
    // entries, which is the only place it can go -- a reader walks up from the
    // entries and expects the terminator when it runs off the end.
    //
    // `put_word` writes at `cur - 8` and then decrements, so each successive call
    // lands one word *lower*. That means the call order is the reverse of the
    // memory order: to get entries ascending above a terminator, the terminator
    // is pushed first and the entries are then pushed back to front.
    //
    // Iterating forward instead lays the array out reversed, which is a very quiet
    // failure. mlibc's `parse_exec_stack` does `sp += argc` and then expects the
    // NULL there; with a reversed array that word is argv[argc-1]'s pointer
    // rather than a terminator, so a program receives its arguments back to front
    // and walks off the end of its own argv. busybox handed `[/bin/busybox
    // --install -s /bin]` that way, took argv[0] as "/bin", and exited 127
    // reporting "bin: applet not found" -- a symptom pointing at the applet table
    // when the cause was the stack layout.
    put_word(&mut cur, 0, aspace);
    for p in env_ptrs.iter().take(n_env).rev() {
        put_word(&mut cur, *p, aspace);
    }
    // argv, same shape.
    put_word(&mut cur, 0, aspace);
    for p in arg_ptrs.iter().take(n_arg).rev() {
        put_word(&mut cur, *p, aspace);
    }
    // argc, the word the initial `%rsp` points at.
    put_word(&mut cur, n_arg as u64, aspace);

    crate::log::kdebug!(
        "elf: entry stack argc={} argv@{:#x} envp={} words={} rsp={:#x} (align {})",
        n_arg,
        if n_arg > 0 { arg_ptrs[0] } else { 0 },
        n_env,
        words,
        cur,
        cur % 16,
    );
    // The arguments themselves, not just how many there are.
    //
    // A count and a base address are enough to say "something was staged" and not
    // enough to say what, and a program that receives the wrong argv produces a
    // symptom several steps downstream: busybox given the wrong one treats
    // "--install" as an applet name and exits 127, which reads as a spawn failure
    // rather than as bad data. Truncated, because a program with a long argument
    // vector should not be able to flood the console from a debug line.
    if !args.is_empty() {
        let shown: usize = args.len().min(8);
        crate::log::kdebug!(
            "elf: argv = [{}]{}",
            args[..shown].join(" "),
            if shown < args.len() { " ..." } else { "" }
        );
    }

    // The libc entry path reads the entry stack and then calls into C, where a
    // misaligned `%rsp` faults on the first aligned SSE move. Cheap to assert,
    // and the alternative is a crash a long way from its cause.
    debug_assert_eq!(
        cur % 16,
        0,
        "entry-stack %rsp must be 16-byte aligned at process entry"
    );
    if cur % 16 != 0 {
        crate::log::kwarn!("elf: entry-stack %rsp is {}-byte misaligned", cur % 16);
    }

    // The caller jumps to `e_entry` with this `%rsp`, so hand back the aligned
    // cursor rather than `stack_end`.
    cur
}

/// What the loader produced for one process image.
///
/// A struct rather than a tuple because these four values travel together
/// through `load` -> `user::build_image` -> the scheduler, and a tuple that
/// grows a fourth element is exactly the kind of thing that gets destructured in
/// the wrong order at a call site nobody re-reads.
pub struct Loaded {
    /// The new address space's page-table root.
    pub cr3: usize,
    /// `e_entry`, the first instruction to run.
    pub entry: usize,
    /// The initial `%rsp`: the word holding `argc`.
    pub rsp: usize,
    /// The address the thread pointer (`%fs`) starts at, or 0 for an image with
    /// no `PT_TLS`.
    pub fs_base: usize,
}

/// Round `v` up to a multiple of `a`. Not a bitmask, because `PT_TLS`'s
/// `p_align` is only *promised* to be a power of two and a malformed image
/// should not turn an alignment into a wild pointer.
fn align_up(v: usize, a: usize) -> usize {
    if a == 0 {
        return v;
    }
    v.div_ceil(a) * a
}

/// Lay out a thread block for `tls` and return the address of its thread-control
/// block, which is also the value `%fs` takes.
///
/// The shape is fixed by the toolchain, not chosen here. A statically linked
/// image addresses its `__thread` variables as *local-exec*: `mov %fs:offset`,
/// with a negative offset measured from the thread pointer, which the linker
/// computed assuming the thread pointer sits at the top of the thread-local
/// block. So the block must be laid out with its data at the bottom and the
/// thread-control block immediately above, and `%fs` must point at the top --
/// `tls_base + mem_sz == fs_base` exactly. mlibc's own dynamic loader builds
/// the same shape in `allocateTcb`, and mlibc's `get_current_tcb` is a bare
/// `mov %fs:0`, so there is no second opinion available at run time.
///
/// That is also why the image's own `.tbss` is not used in place. It is the
/// right size, but it sits at the bottom of the image's writable segment with
/// the program's own `.bss` directly above it, so a thread-control block placed
/// there would overwrite the program's data. The block is therefore relocated
/// into fresh pages, with the image's initialized `.tdata` copied in.
///
/// Returns 0 when the image has no `PT_TLS` at all. That is not a failure: an
/// image with no thread-local variables has no thread pointer, and leaving `%fs`
/// at zero says so. mlibc's `this_tid` copes with it by refusing to read a
/// thread-control block it knows is not there.
fn map_thread_block(
    aspace: &mut vmm::AddressSpace,
    tls: &Tls,
    image_name: &str,
) -> Result<usize, i64> {
    // One alignment to satisfy both, so the block's data starts aligned *and* the
    // thread-control block above it is aligned. mlibc's `allocateTcb` does the
    // same thing and calls it `alignOverhead`.
    let overhead = TCB_ALIGN.max(tls.align);
    let total = tls
        .mem_sz
        .checked_add(TCB_RESERVE)
        .and_then(|v| v.checked_add(overhead))
        .ok_or(ENOMEM)?;
    let frames = total.div_ceil(PAGE);

    let va = crate::memory::user_map::reserve_image_va(frames).ok_or(ENOMEM)?;
    for i in 0..frames {
        let phys = match pmm::alloc_frame() {
            Some(p) => p,
            None => {
                crate::log::kwarn!(
                    "elf: no frame for {image_name}'s thread block (page {i}/{frames})"
                );
                return Err(ENOMEM);
            }
        };
        // SAFETY: the region is reserved for this image alone and was never
        // reachable, so zeroing the frame before it is mapped is what makes the
        // `.tbss` tail and the thread-control block read as zero.
        unsafe {
            core::ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, PAGE);
            aspace.map_page(
                va + i * PAGE,
                phys,
                vmm::USER_ACCESSIBLE | vmm::WRITABLE | vmm::NO_EXECUTE,
            );
        }
    }

    // The invariant: thread-local data ends exactly where the thread-control
    // block begins. Everything else in this layout follows from it.
    let fs_base = align_up(va + tls.mem_sz, overhead);
    let tls_base = fs_base - tls.mem_sz;

    // Copy the image's initialized thread-local data into place. Read through
    // the address space rather than the file buffer: the loader has already
    // mapped and relocated the image, and a `PT_TLS` segment's `p_offset` is not
    // tracked here. The copy is byte-at-a-time, which is not the shape one would
    // choose for a hot path but is the shape that cannot be wrong about page
    // boundaries -- and it runs once per process spawn over a block that is
    // usually well under a kilobyte.
    for i in 0..tls.file_sz {
        let src = tls.vaddr + i;
        let Some((phys, _)) = aspace.translate(src) else {
            crate::log::kwarn!(
                "elf: {image_name}'s PT_TLS initial data at {src:#x} is not mapped; \
                 thread-local variables start zeroed"
            );
            break;
        };
        // SAFETY: `src` translated, and the destination is the freshly mapped,
        // zeroed thread block this function owns.
        unsafe {
            let byte = (phys_to_virt(phys) as *const u8).read();
            ((tls_base + i) as *mut u8).write(byte);
        }
    }

    crate::log::kdebug!(
        "elf: {image_name} thread block: tls {tls_base:#x}+{:#x} .. fs {fs_base:#x} \
         (tcb reserved {TCB_RESERVE:#x}, tls align {:#x})",
        tls.mem_sz,
        tls.align
    );
    Ok(fs_base)
}

/// Load embedded ELF `img` into a fresh address space, ready to jump to
/// `e_entry` with `%rsp` pointing at the staged `argc`. Returns
/// `(cr3, entry, rsp)`.
///
/// `args`/`env` become the process's `argv`/`envp` on the initial stack, so a
/// libc-based program discovers them the usual way.
pub fn load(
    img: &[u8],
    stack_base: usize,
    stack_end: usize,
    args: &[String],
    env: &[String],
    image_name: &str,
) -> Result<Loaded, i64> {
    let h = parse_header(img).ok_or(ENOEXEC)?;
    let mut phdrs = parse_phdrs(img, &h).ok_or(ENOEXEC)?;

    // The ELF identity and program headers may live in a file page before the
    // first PT_LOAD (our linker script starts it at file offset PAGE). Map
    // that lead-in so AT_PHDR points at real memory, like the Linux loader.
    let first = phdrs.segs[0];
    if first.file_off > 0 && first.file_off <= PAGE {
        phdrs.segs.insert(
            0,
            Seg {
                file_off: 0,
                vaddr: first.vaddr - first.file_off,
                file_sz: first.file_off,
                mem_sz: first.file_off,
                flags: PF_R | PF_X,
            },
        );
    }
    let segs = phdrs.segs;
    let tls = phdrs.tls;

    // Reject overlapping page ranges between loads (each is mapped as its own
    // set of fresh frames).
    let mut ranges: Vec<(usize, usize)> = segs
        .iter()
        .map(|s| (s.vaddr & !(PAGE - 1), (s.vaddr + s.mem_sz + PAGE - 1) & !(PAGE - 1)))
        .collect();
    ranges.sort_unstable();
    for w in ranges.windows(2) {
        if w[0].1 > w[1].0 {
            return Err(ENOEXEC);
        }
    }

    // The entry point must live inside an executable load.
    if h.entry == 0
        || !segs.iter().any(|s| {
            s.flags & PF_X != 0 && s.vaddr <= h.entry && h.entry < s.vaddr + s.mem_sz
        })
    {
        return Err(ENOEXEC);
    }

    let mut aspace = vmm::AddressSpace::new();
    aspace.clone_kernel_half(vmm::kernel_root());

    for s in segs.iter() {
        map_segment(&mut aspace, img, s)?;
    }

    // Dynamic linking. We need the PT_DYNAMIC table's file span; its vaddr
    // sits inside the mapped image but the loader reads it from the buffer.
    let dyn_file = phdrs.dynamic_file.ok_or(ENOEXEC)?;
    if dyn_file + 16 > img.len() {
        return Err(ENOEXEC);
    }
    let nrel = relocate(&aspace, img, &segs, dyn_file)?;

    // Thread block, before the stack: an image with thread-local variables needs
    // `%fs` pointing at a real block from its very first instruction, and the
    // libc's entry code reads it before it has allocated anything for itself.
    let fs_base = match tls {
        Some(t) => map_thread_block(&mut aspace, &t, image_name)?,
        None => 0,
    };

    // User stack: read/write, non-executable.
    let stack_pages = (stack_end - stack_base) / PAGE;
    for i in 0..stack_pages {
        let phys = match pmm::alloc_frame() {
            Some(p) => p,
            None => {
                // The image mapped, then the stack did not. That ordering is
                // informative: the stack is allocated last, so this is what a
                // nearly-full allocator looks like rather than an image that is
                // too big. Both are "out of memory" and neither is fixable by
                // making the program smaller.
                let (used, total) = pmm::stats();
                crate::log::kwarn!(
                    "elf: no frame for the user stack (page {}/{}), pmm {used}/{total}",
                    i,
                    stack_pages
                );
                return Err(ENOMEM);
            }
        };
        // SAFETY: stack region freshly reserved; frame zeroed via alias.
        unsafe {
            aspace.map_page(
                stack_base + i * PAGE,
                phys,
                vmm::USER_ACCESSIBLE | vmm::WRITABLE | vmm::NO_EXECUTE,
            );
            core::ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, PAGE);
        }
    }

    // AT_PHDR: the phdr table sits at file offset `h.phoff`, which the lead-in
    // (or the first load, for ET_EXEC-style gaps of zero) maps to a vaddr we
    // can hand the process.
    let ph_vaddr = {
        let lead = segs[0];
        lead.vaddr + (h.phoff.saturating_sub(lead.file_off))
    };
    let rsp = write_entry_stack(
        &aspace,
        stack_base,
        stack_end,
        ph_vaddr,
        h.phnum,
        h.entry,
        fs_base,
        args,
        env,
    );

    crate::log::kinfo!(
        "elf: loaded {} bytes, {} load segments, {} relocations applied, entry {:#x}, cr3={:#x}",
        img.len(),
        segs.len(),
        nrel,
        h.entry,
        aspace.root()
    );
    Ok(Loaded {
        cr3: aspace.root(),
        entry: h.entry,
        rsp,
        fs_base,
    })
}