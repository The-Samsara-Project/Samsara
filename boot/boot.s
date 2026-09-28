; SPDX-License-Identifier: GPL-3.0-or-later
; Copyright (C) 2026 Harsh Nikarsa
;
; Samsara boot code.
;
; GRUB loads this image as a Multiboot2 (ELF64) kernel at 1 MiB physical and
; jumps to `_start` in 32-bit protected mode with:
;   eax = 0x36D76289 (Multiboot2 bootloader magic)
;   ebx = physical address of the Multiboot2 information structure
;
; This stub:
;   1. builds identity + higher-half page tables (2 MiB pages, first GiB),
;   2. enables PAE / LME / paging to enter 64-bit long mode,
;   3. installs a minimal 64-bit GDT,
;   4. jumps into the high virtual half and calls the Rust `kmain(magic, mbi)`.

%define KBASE       0xFFFFFFFF80000000
%define KBASE_LOW    0x80000000
%define MB2_MAGIC   0x36D76289
%define STACK_SIZE  0x10000

; ---------------------------------------------------------------------------
; Multiboot2 header (must be 8-byte aligned, within the first 32 KiB)
; ---------------------------------------------------------------------------
section .multiboot_header align=8

header_start:
    dd 0xE85250D6                       ; magic
    dd 0                                ; architecture: protected mode i386
    dd header_end - header_start        ; header length
    dd -(0xE85250D6 + 0 + (header_end - header_start)) ; checksum
            ; --- framebuffer request (1024x768x32 linear RGB) ---
    align 8
    ; Multiboot2 header tag type 5 is the framebuffer request.  Type 6 is
    ; module alignment, so using it silently prevented GRUB from receiving
    ; this request.
    dw 5                                ; type: framebuffer
    dw 0                                ; flags
    dd 20                               ; tag size (alignment padding excluded)
    dd 1024                             ; width
    dd 768                              ; height
    dd 32                               ; depth (bits per pixel)
            ; --- end tag ---
    align 8
    dw 0                                ; type: END
    dw 0                                ; flags
    dd 8                                ; size
header_end:

; ---------------------------------------------------------------------------
; Early boot data (accessed through the identity map via `- KBASE` aliases)
; ---------------------------------------------------------------------------
section .boot.bss nobits alloc noexec write align=4096

pml4:               resb 4096           ; PML4[0]   = pdpt_common (identity)
                                        ; PML4[511] = pdpt_common (higher half)
pdpt_common:        resb 4096           ; PDPT[0]   = pd_ident
                                        ; PDPT[511] = pd_kern
pd_ident:           resb 4096           ; phys 0 .. 1 GiB at virt 0 ..
pd_kern:            resb 4096           ; phys 0 .. 1 GiB at KBASE ..

alignb 16
boot_stack_bottom:  resb STACK_SIZE
boot_stack_top:

; ---------------------------------------------------------------------------
; 32-bit protected mode entry point
; ---------------------------------------------------------------------------
section .boot.text exec progbits
bits 32

global _start
extern kmain

_start:
    cli
    cld

    mov esp, boot_stack_top     ; low alias of the boot stack
    xor ebp, ebp

    ; Preserve Multiboot2 register state for the 64-bit world.
    mov edi, eax                        ; edi = bootloader magic
    mov esi, ebx                        ; esi = MBI physical address

    ; ---- zero the four static page tables -----------------------------
    xor eax, eax
    mov ecx, (4 * 4096) / 4
    mov edx, pml4
.zero_tables:
    mov [edx], dword 0
    add edx, 4
    loop .zero_tables

    ; ---- build the page tables ----------------------------------------
    ; PML4[0] and PML4[511] -> pdpt_common
    mov eax, pdpt_common
    or  eax, 0x03                                  ; present | writable
    mov [(pml4) + 0*8], eax
    mov [(pml4) + 511*8], eax

    ; PDPT[0] -> identity 1 GiB.
    ; PDPT[510] -> higher half: PML4[511] window starts at 0xFFFFFF8000000000
    ; and slot 510 begins at 0xFFFFFFFF80000000 where the kernel is linked.
    mov eax, pd_ident
    or  eax, 0x03
    mov [(pdpt_common) + 0*8], eax
    mov eax, pd_kern
    or  eax, 0x03
    mov [(pdpt_common) + 510*8], eax

    ; Fill both page directories with 512 x 2 MiB huge pages.
    xor ecx, ecx
.fill_pd_ident:
    mov eax, ecx
    shl eax, 21                         ; frame base = i * 2 MiB
    or  eax, 0x83                       ; present | writable | 2MiB page
    mov [(pd_ident) + ecx*8], eax
    inc ecx
    cmp ecx, 512
    jl .fill_pd_ident

    xor ecx, ecx
.fill_pd_kern:
    mov eax, ecx
    shl eax, 21
    or  eax, 0x83
    mov [(pd_kern) + ecx*8], eax
    inc ecx
    cmp ecx, 512
    jl .fill_pd_kern

    ; ---- enable paging -------------------------------------------------
    mov eax, cr4
    or  eax, (1 << 5) | (1 << 7) | (1 << 4)   ; PAE | PSE | PGE
    mov cr4, eax

    mov eax, pml4
    mov cr3, eax

    mov ecx, 0xC0000080                 ; IA32_EFER
    rdmsr
    or  eax, (1 << 8)                   ; LME: long mode enable
    wrmsr

    mov eax, cr0
    or  eax, 0x80010001                 ; PG | PE (+ reserved ET bit)
    mov cr0, eax

    lgdt [boot_gdt_descriptor]


    jmp 0x08:long_mode_entry            ; far jump: enter 64-bit code

; ---------------------------------------------------------------------------
; Minimal 64-bit GDT: null | 0x08 code64 | 0x10 data64
; ---------------------------------------------------------------------------
section .boot.data progbits write
align 8

boot_gdt:
    dq 0                                ; null descriptor
    dq 0x00AF9A000000FFFF               ; 0x08: 64-bit code, ring 0
    dq 0x00CF92000000FFFF               ; 0x10: data, ring 0
boot_gdt_end:

boot_gdt_descriptor:
    dw boot_gdt_end - boot_gdt - 1
    dd boot_gdt                 ; linear (identity-mapped) address

; ---------------------------------------------------------------------------
; 64-bit trampoline (still executing in the identity-mapped low half)
; ---------------------------------------------------------------------------
section .boot.text exec progbits
bits 64

long_mode_entry:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov fs, ax
    mov gs, ax

    mov rsp, boot_stack_top


    movabs rax, high_half_entry
    jmp rax                              ; leap into the higher half

; ---------------------------------------------------------------------------
; Higher half: hand control to the Rust kernel.
;
; Everything from here on runs on a higher-half stack so the later CR3
; switch in the VMM (which drops the identity map) is invisible.
; ---------------------------------------------------------------------------
section .text exec progbits
bits 64

extern kernel_boot_stack_top

high_half_entry:
    mov rsp, kernel_boot_stack_top      ; real (higher-half) kernel stack
    xor ebp, ebp

    ; rdi = bootloader magic, rsi = MBI physical address (set in 32-bit land)
    call kmain

.hang:
    cli
    hlt
    jmp .hang

section .bss nobits align=16

global kernel_boot_stack_bottom
kernel_boot_stack_bottom: resb STACK_SIZE
kernel_boot_stack_top:
