<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- Copyright (C) 2026 Harsh Nikarsa -->

# Samsara Architecture

Samsara is an independent 64-bit micro-kernel. It shares no ABI, driver
model or syscall surface with Linux or any other kernel.

## Boot path

```
GRUB (Multiboot2)
  └─ loads ELF64 kernel at physical 1 MiB, jumps to _start
       └─ boot/boot.s (32-bit)
            ├─ builds static page tables: identity + higher-half map
            │    PML4[0]   -> identity 0 .. 1 GiB      (boot only)
            │    PML4[511] -> physmap/kernel half      (permanent)
            ├─ enables PAE/LME/paging -> long mode
            ├─ installs minimal 64-bit GDT
            └─ far-jumps to the higher half and calls kmain(magic, mbi)
```

The kernel is linked in the higher half at `0xFFFFFFFF80000000`; GRUB's
Multiboot2 ELF loader uses the `p_paddr` values produced by the linker
script (`kernel/linker.ld`).

## Address space layout

| Range | Purpose |
|---|---|
| `0x0000_0000_0000_0000 .. 0x0000_7FFF_FFFF_FFFF` | userspace (reserved) |
| `0xFFFF_FF00_0000_0000` | kernel heap (64 MiB) |
| `0xFFFF_FFFF_8000_0000 .. +1 GiB` | physical map of low memory + kernel image |

After boot the VMM builds fresh page tables that keep only the upper-half
PML4 entries — the boot-time identity mapping is dropped on purpose, so any
accidental use of physical addresses faults immediately.

## Subsystems

* **`memory::pmm`** — bitmap frame allocator seeded from the Multiboot2
  memory map. The kernel image and the bitmap itself are permanently
  reserved. The allocator currently manages the first GiB of RAM.
* **`memory::vmm`** — four-level page-table manager with on-demand
  intermediate table allocation (`map_page`, `translate`, `switch_to`).
* **`memory::heap`** — first-fit linked-list allocator with block coalescing,
  exported through `GlobalAlloc` so standard `alloc` collections work.
* **`interrupts`** — kernel-only GDT with ring-3 descriptors pre-provisioned,
  TSS with an IST emergency stack, full IDT coverage for x86-64 exceptions
  (page faults report CR2), remapped 8259 PICs, and a PIT ticking at 100 Hz.
* **`abi`** — the stable system call contract: MSR-programmed
  `syscall`/`sysret`, a dedicated kernel stack, full register save/restore,
  and a registration table for in-kernel handlers.
* **`sync`** — hand-rolled spinlocks and write-once cells; zero external
  dependencies anywhere in the tree.

## Design rules

1. No external crates. Everything is implemented in-tree.
2. No Linux compatibility layer; POSIX compliance is provided by *Samsara's*
   own ABI.
3. Drivers that must run outside the kernel will be isolated userspace
   processes ; everything else stays
   strictly in the kernel.
