/* SPDX-License-Identifier: GPL-3.0-or-later
 * Copyright (C) 2026 Harsh Nikarsa
 *
 * Linker script for ring-3 Samsara user programs.
 *
 * Every user process is loaded into its own address space at the same
 * virtual base and runs flat (no ELF loader); the kernel copies this image
 * into read-mostly pages and iretq's to _start.
 */

OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

USER_BASE = 0x110000;   /* 1 MiB + 64 KiB; clear of the 1 MiB low area */

SECTIONS
{
    . = USER_BASE;
    .text : ALIGN(4096)
    {
        *(.text .text.*)
    }
    .rodata : ALIGN(4096)
    {
        *(.rodata .rodata.*)
    }
    .data : ALIGN(4096)
    {
        *(.data .data.*)
        *(.got .got.*)
        *(.got.plt .got.plt.*)
    }
    .bss : ALIGN(4096)
    {
        *(.bss .bss.*)
        *(COMMON)
    }
    /DISCARD/ :
    {
        *(.eh_frame)
        *(.eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
        
        
        
    }
}