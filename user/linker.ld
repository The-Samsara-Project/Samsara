/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * Linker script for ring-3 Samsara user programs.
 *
 * Every user process is a static-position-independent ELF (ET_DYN, -pie).
 * The kernel ELF loader maps the PT_LOAD segments and acts as the dynamic
 * linker: it resolves .rela.dyn/.rela.plt (R_X86_64_RELATIVE at minimum)
 * against the linked bias and applies PT_GNU_RELRO before the process runs.
 * The dynamic tables (.dynamic/.dynsym/.dynstr/.hash/.rela*) are kept in the
 * image (mapped inside the PT_LOAD segments) so the loader can operate on the
 * same in-memory layout a userspace ld.so would see.
 */

OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

/* Link-time virtual base; the loader adds a runtime bias on top. */
USER_BASE = 0x110000;

PHDRS
{
    text    PT_LOAD FLAGS(5);   /* R E */
    rodata  PT_LOAD FLAGS(4);   /* R   */
    data    PT_LOAD FLAGS(6);   /* R W */
    dynamic PT_DYNAMIC FLAGS(6);
}

SECTIONS
{
    . = USER_BASE;

    .text : ALIGN(4096)
    {
        /* The kernel jumps to e_entry, so _start must sit at the entry
         * address. With LTO every function shares one .text unless
         * function-sections is on; keep the entry first regardless. */
        KEEP(*(.text._start))
        KEEP(*(.text.start))
        *(.text .text.*)
        *(.plt) *(.plt.sec)
    } :text

    .rodata : ALIGN(4096)
    {
        *(.rodata .rodata.*)
    } :rodata

    .dynsym : ALIGN(8)
    {
        *(.dynsym)
    } :rodata

    .dynstr : ALIGN(8)
    {
        *(.dynstr)
    } :rodata

    .gnu.hash : ALIGN(8)
    {
        *(.gnu.hash)
    } :rodata

    .hash : ALIGN(8)
    {
        *(.hash)
    } :rodata

    .data : ALIGN(4096)
    {
        *(.data .data.*)
        *(.data.rel.ro .data.rel.ro.*)
        *(.got .got.*)
        *(.got.plt .got.plt.*)
    } :data

    .dynamic : ALIGN(8)
    {
        *(.dynamic)
    } :dynamic

    /* Relocation tables must live inside a PT_LOAD so their d_ptr addresses
     * resolve; the loader reads them from the mapped image like ld.so would. */
    .rela : ALIGN(8)
    {
        *(.rela.dyn)
        *(.rela.plt)
    } :data

    .bss : ALIGN(4096)
    {
        *(.bss .bss.*)
        *(COMMON)
    } :data

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
        *(.symtab)
        *(.strtab)
    }
}