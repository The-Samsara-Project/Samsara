# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Samsara build orchestration.
#
#   make          - build the kernel image (release)
#   make iso      - produce a bootable GRUB ISO
#   make mlibc    - build the mlibc sysroot port (see ports/mlibc/README)
#   make run      - boot the ISO in QEMU with serial on stdio
#   make debug    - QEMU with a GDB stub listening on :1234
#   make clean    - remove all build artifacts

KERNEL_ELF := target/samsara.elf
ISO        := samsara.iso
ISODIR     := isodir

LLD := ld.lld

# Prefer the rustup-managed toolchain over any distro rustc.
export PATH := $(HOME)/.cargo/bin:$(PATH)

# Ring-3 user programs come from three places:
#   * boot servers — their own crates under user/servers/ (consoled, inputd).
#     The kernel launches these during boot; each links its own linker script.
#   * the utility multi-call binary — user/samutils/ (the real commands the
#     shell runs); spawned on demand with an argv.
#   * example programs — crates under user/nutcracker-rt/examples/ (self-tests,
#     the shell, the terminal, the installer).
# All are static-position-independent ELFs embedded in the kernel, which acts
# as their dynamic linker (mapping PT_LOADs + resolving .rela). Their flags
# live in user/.cargo/config.toml (built from `user/`); the kernel's
# code-model=kernel config is scoped to kernel/.cargo/config.toml.
USER_SERVERS   := consoled inputd
USER_UTILS     := samutils
USER_EXAMPLES  := hello forkx exectst pipetest credtst signaltst polltest termiostst sh term installer

.PHONY: all kernel iso run debug clean user-bins mlibc fbterm

all: iso

# Build mlibc (Nutcracker's libc port) into build/sysroot. Purely a convenience
# wrapper around ports/mlibc/build.sh.
mlibc:
	./ports/mlibc/build.sh

# Build fbterm (the ported Linux terminal emulator) into build/fbterm-build.
# Separate from `all` on purpose: it needs a built mlibc sysroot, so folding it
# into the default target would make a clean tree require the libc port before
# anything else could be built. Run `make mlibc fbterm` for a terminal binary.
fbterm:
	./ports/fbterm/build.sh

# Build the Nutcracker user images (example programs and servers) as static-PIE
# ELFs for the kernel loader. `chello` is built here too even though it is the
# one C image rather than a Rust one: the kernel embeds every program in its
# image, so a missing user-*.elf is a build failure, not a skipped step.
user-bins:
	cd user && CARGO_TARGET_DIR="$(CURDIR)/target/user" cargo build --release --target x86_64-unknown-none --examples --bins
	@for b in $(USER_EXAMPLES); do cp \
	    target/user/x86_64-unknown-none/release/examples/$$b target/user-$$b.elf; done
	@for b in $(USER_SERVERS); do cp \
	    target/user/x86_64-unknown-none/release/$$b target/user-$$b.elf; done
	@for b in $(USER_UTILS); do cp \
	    target/user/x86_64-unknown-none/release/$$b target/user-$$b.elf; done
	@sh user/build-chhello.sh
	@# fbterm is a ported C++ program built by its own script against the mlibc
	@# sysroot, not a cargo target. Copied in only when it has been built, so a
	@# tree without a sysroot still produces a working ISO; `make fbterm` builds
	@# it. Not in the kernel's program table yet -- see ports/fbterm/README.md for
	@# the outstanding ttyname check that keeps it from running yet.
	@if [ -f build/fbterm-build/fbterm.elf ]; then \
	    cp build/fbterm-build/fbterm.elf target/user-fbterm.elf; \
	fi
	@ls -l target/user-*.elf

kernel: user-bins
	cd kernel && CARGO_TARGET_DIR="$(CURDIR)/target" cargo build --release
	nasm -f elf64 -g -F dwarf boot/boot.s -o target/boot.o

$(KERNEL_ELF): kernel
	@mkdir -p target
	$(LLD) -nostdlib -static --gc-sections --no-dynamic-linker \
	       -z noexecstack -T kernel/linker.ld \
	       target/boot.o \
	       target/x86_64-unknown-none/release/libsamsara.a \
	       -o $(KERNEL_ELF)

iso: $(KERNEL_ELF)
	rm -rf $(ISODIR)
	mkdir -p $(ISODIR)/boot/grub
	cp $(KERNEL_ELF) $(ISODIR)/boot/samsara.bin
	cp grub/grub.cfg $(ISODIR)/boot/grub/grub.cfg
	grub-mkrescue -o $(ISO) $(ISODIR) 2>/dev/null
	@echo "created $(ISO)"

run: iso
# `+rdseed,+rdrand` asks QEMU to present the hardware entropy instructions the
# host CPU has. The kernel probes for them (entropy.rs) and prefers them over the
# timing-jitter fallback, which is measurably weaker; without this flag `make run`
# boots with the fallback and `getrandom` still works, just less well.
	qemu-system-x86_64 \
	    -cdrom $(ISO) \
	    -cpu qemu64,+rdseed,+rdrand \
	    -serial stdio \
	    -display none \
	    -no-reboot

debug: iso
	qemu-system-x86_64 \
	    -cdrom $(ISO) \
	    -cpu qemu64,+rdseed,+rdrand \
	    -serial stdio \
	    -display none \
	    -no-reboot \
	    -s -S

clean:
	cargo clean 2>/dev/null || true
	rm -rf target/boot.o $(ISODIR) $(ISO) $(KERNEL_ELF)
