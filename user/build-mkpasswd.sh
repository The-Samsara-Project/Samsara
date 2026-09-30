#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Build user/mkpasswd.c against the Samsara mlibc sysroot (see ports/mlibc).
#
# No custom linker script. lld's default one already lays out PT_LOAD, PT_TLS,
# PT_DYNAMIC, GNU_RELRO, the init arrays and .bss correctly -- including putting
# PT_TLS *inside* the writable PT_LOAD, which the loader needs. Its only problem
# for Samsara is that it starts the image at vaddr 0, and the loader rejects any
# PT_LOAD at or below the first page (see kernel/src/elf.rs). `--image-base`
# shifts the whole image above the first page while leaving the default layout
# untouched. A hand-written script with explicit PHDRS gets this wrong in
# several ways at once -- an unmapped PT_TLS, a `.bss` outside every load -- so
# the default script is strictly better here.
#
# Two other flags are forced by the kernel loader:
#
#   * `syscall` is bound to `__do_syscall4` with --defsym, because mlibc's
#     <unistd.h> does not export the raw syscall entry and bits/syscall.h is an
#     internal header a program should not include;
#   * `__dso_handle` is the static-link anchor that the C++ objects inside
#     libc.a reference. A hosted toolchain injects it for a C program linked
#     against a C++ archive; a freestanding link must supply it explicitly.
set -e

ROOT=$(cd "$(dirname "$0")/.." && pwd)
SYSROOT="$ROOT/build/sysroot/usr"
LIB="$SYSROOT/lib/x86_64-pc-samsara"
FREESTND="$ROOT/build/mlibc-src/subprojects/freestnd-c-hdrs/x86_64/include"

if [ ! -f "$LIB/libc.a" ]; then
	echo "chello: no mlibc sysroot at $LIB -- run 'make mlibc' first" >&2
	exit 1
fi

ANCHOR=$(mktemp -d)
trap 'rm -rf "$ANCHOR"' EXIT
printf 'void *__dso_handle = 0;\n' > "$ANCHOR/dso.c"

# shellcheck disable=SC2086
clang --target=x86_64-pc-samsara \
	-ffreestanding -nostdlib -static-pie -fPIE \
	-fno-builtin -D_GNU_SOURCE -O2 \
	-nostdinc \
	-isystem "$SYSROOT/include" \
	-isystem "$FREESTND" \
	-o "$ROOT/target/user-mkpasswd.elf" \
	"$ROOT/user/mkpasswd.c" "$ANCHOR/dso.c" \
	"$LIB/crt1.o" "$LIB/libc.a" \
	-Wl,--image-base=0x110000 \
	-Wl,--defsym=syscall=__do_syscall4 \
	-Wl,--build-id=none

echo "mkpasswd: built target/user-mkpasswd.elf"
