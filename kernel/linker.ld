/* SPDX-License-Identifier: GPL-3.0-or-later */
/* Copyright (C) 2026 Harsh Nikarsa */
/*
 * Samsara kernel linker script.
 *
 * The image is linked in the higher half at KERNEL_VIRT. The physical load
 * address (LMA) of every section is VMA - PHYSMAP_BASE, so GRUB's Multiboot2
 * ELF loader places the image at the matching low physical addresses while
 * the code itself runs at its high virtual addresses.
 */

OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

KERNEL_VIRT   = 0xFFFFFFFF80000000;
PHYSMAP_BASE  = 0xFFFFFFFF80000000;
KERNEL_LMA    = 0x100000;               /* 1 MiB, where GRUB loads us */

SECTIONS
{
    /* ---- physical layout anchor ------------------------------------ */
    . = KERNEL_LMA;
    __kernel_lma_start = .;

    /*
     * Early-boot code and data stay identity-mapped: they are linked AND
     * loaded at their physical addresses so the 32-bit entry stub can run
     * before paging is enabled.
     */
    .boot : ALIGN(4K) {
        KEEP(*(.multiboot_header))
        *(.boot.text)
        *(.boot.data)
    }

    .boot.bss (NOLOAD) : ALIGN(4K) {
        *(.boot.bss)
    }

    /* ---- higher-half kernel ---------------------------------------- */
    . += KERNEL_VIRT;
    __kernel_vma_start = .;

    .text : AT(ADDR(.text) - KERNEL_VIRT) ALIGN(4K) {
        __text_start = .;
        *(.text .text.*)
        __text_end = .;
    }

    .rodata : AT(ADDR(.rodata) - KERNEL_VIRT) ALIGN(4K) {
        __rodata_start = .;
        *(.rodata .rodata.*)
        __rodata_end = .;
    }

    .data : AT(ADDR(.data) - KERNEL_VIRT) ALIGN(4K) {
        __data_start = .;
        *(.data .data.*)
        *(.got .got.*)
        __data_end = .;
    }

    .bss (NOLOAD) : AT(ADDR(.bss) - KERNEL_VIRT) ALIGN(4K) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    __kernel_vma_end = ALIGN(., 4K);
    __kernel_lma_end = __kernel_vma_end - KERNEL_VIRT;

    __kernel_image_size = __kernel_lma_end - __kernel_lma_start;

    /DISCARD/ : {
        *(.eh_frame*)
        *(.note*)
        *(.comment*)
    }
}
