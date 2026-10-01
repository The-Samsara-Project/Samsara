# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Samsara build orchestration.
#
#   make          - build the kernel image (release)
#   make iso      - produce a bootable GRUB ISO
#   make mlibc    - build the mlibc sysroot port (see ports/mlibc/README)
#   make busybox  - build the busybox port (see ports/busybox/README)
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
#   * the userland — the busybox port (ports/busybox/), which is also what
#     `/bin/sh` is; spawned on demand with an argv and dispatched by argv[0].
#   * system programs — crates under user/servers/ (the console server, the
#     keyboard driver, the installer, the getty that fronts a terminal).
#   * example programs — crates under user/nutcracker-rt/examples/ (self-tests
#     and the shell). Nothing here is needed to boot or to be usable: they exist
#     to be run and checked, not to set the system up.
# All are static-position-independent ELFs embedded in the kernel, which acts
# as their dynamic linker (mapping PT_LOADs + resolving .rela). Their flags
# live in user/.cargo/config.toml (built from `user/`); the kernel's
# code-model=kernel config is scoped to kernel/.cargo/config.toml.
USER_SERVERS   := consoled inputd installer
USER_EXAMPLES  := hello forkx exectst pipetest credtst signaltst polltest termiostst sh

.PHONY: all kernel iso run run-fbterm debug clean user-bins fbterm-run
.PHONY: mlibc fbterm busybox

all: iso

# The port sources, enumerated at parse time so a change to any file in a port
# directory invalidates that port's stamp. The `build/` exclusion matters:
# ports/mlibc/build.sh is itself a git clone of mlibc, and listing its tens of
# thousands of files would make the dependency set enormous and unstable.
MLIBC_INPUTS  := $(shell find ports/mlibc -type f -not -path '*/build/*' 2>/dev/null)
FBTERM_INPUTS := $(shell find ports/fbterm -type f -not -path '*/build/*' 2>/dev/null)
BUSYBOX_INPUTS := $(shell find ports/busybox -type f -not -path '*/build/*' 2>/dev/null)

# Stamps rather than phony prerequisites, so `make` does not rebuild a libc on
# every invocation. Both build scripts are slow and both are deterministic, so
# re-running one whose inputs have not changed can only waste a minute.
#
# fbterm depends on the mlibc stamp because it links against the sysroot: a
# rebuilt libc invalidates every object built against the old headers, and
# without this the link would quietly mix two libc builds.
build/.mlibc.stamp: $(MLIBC_INPUTS)
	./ports/mlibc/build.sh
	@mkdir -p build && touch $@

build/.fbterm.stamp: $(FBTERM_INPUTS) build/.mlibc.stamp
	./ports/fbterm/build.sh
	@mkdir -p build && touch $@

# busybox depends on the mlibc stamp for the same reason fbterm does: it is
# linked against the sysroot, so a rebuilt libc invalidates every object built
# against the old headers. It also depends on the fbterm stamp, but only
# through ordering -- both ports exist in the same kernel image and there is no
# reason for one to be staler than the other.
build/.busybox.stamp: $(BUSYBOX_INPUTS) build/.mlibc.stamp
	./ports/busybox/build.sh
	@mkdir -p build && touch $@

# Force a rebuild of either port, ignoring the stamps. For when a build script
# itself has to change behaviour without its own contents changing.
.PHONY: mlibc fbterm mlibc-force fbterm-force
mlibc mlibc-force:
	./ports/mlibc/build.sh
	@mkdir -p build && touch build/.mlibc.stamp

fbterm fbterm-force: build/.mlibc.stamp
	./ports/fbterm/build.sh
	@mkdir -p build && touch build/.fbterm.stamp

busybox busybox-force: build/.mlibc.stamp
	./ports/busybox/build.sh
	@mkdir -p build && touch build/.busybox.stamp

# Build fbterm into a standalone ISO without disturbing the default build.
# Kept for bisecting: a boot that misbehaves with fbterm embedded can be compared
# against one that only has the kernel console.
fbterm-run: build/.fbterm.stamp
	@mkdir -p target/fbterm
	cp build/fbterm-build/fbterm.elf target/user-fbterm.elf
	cd kernel && CARGO_TARGET_DIR="$(CURDIR)/target/fbterm" cargo build --release
	nasm -f elf64 -g -F dwarf boot/boot.s -o target/fbterm/boot.o
	$(LLD) -nostdlib -static --gc-sections --no-dynamic-linker \
	       -z noexecstack -T kernel/linker.ld \
	       target/fbterm/boot.o \
	       target/fbterm/x86_64-unknown-none/release/libsamsara.a \
	       -o target/fbterm/kernel.elf
	rm -rf $(ISODIR)
	mkdir -p $(ISODIR)/boot/grub
	cp target/fbterm/kernel.elf $(ISODIR)/boot/samsara.bin
	cp grub/grub.cfg $(ISODIR)/boot/grub/grub.cfg
	grub-mkrescue -o samsara-fbterm.iso $(ISODIR) 2>/dev/null
	@echo "created samsara-fbterm.iso"
	@echo "run it with: make run-fbterm"

# Build the Nutcracker user images (example programs and servers) as static-PIE
# ELFs for the kernel loader. `chello` is built here too even though it is the
# one C image rather than a Rust one: the kernel embeds every program in its
# image, so a missing user-*.elf is a build failure, not a skipped step.
# The libc and the terminal are prerequisites, not optional extras: the kernel
# embeds fbterm's image unconditionally, so a `make` that skipped the ports would
# fail at the include_bytes! in kernel/src/user.rs. Ordering them here means a
# clean tree still builds with one command, at the cost of needing git, meson,
# ninja and a freestanding clang -- which the mlibc port already required.
user-bins: build/.mlibc.stamp build/.fbterm.stamp build/.busybox.stamp
	cd user && CARGO_TARGET_DIR="$(CURDIR)/target/user" cargo build --release --target x86_64-unknown-none --examples --bins
	@for b in $(USER_EXAMPLES); do cp \
	    target/user/x86_64-unknown-none/release/examples/$$b target/user-$$b.elf; done
	@for b in $(USER_SERVERS); do cp \
	    target/user/x86_64-unknown-none/release/$$b target/user-$$b.elf; done
	@# Programs linked against mlibc rather than built by cargo, because they need
	@# the C library: the self-test, the password hasher and the getty. They are
	@# not cargo targets and so are not in any of the copy loops above; each build
	@# script writes target/user-<name>.elf directly. The Rust programs cannot be
	@# written this way because they do not link mlibc at all.
	@sh user/build-chhello.sh
	@sh user/build-mkpasswd.sh
	@sh user/build-getty.sh
	@# fbterm is a ported C++ program built by ports/fbterm/build.sh against the
	@# mlibc sysroot, not a cargo target, so it is copied into place here. The
	@# kernel embeds it unconditionally as the system terminal.
	@cp build/fbterm-build/fbterm.elf target/user-fbterm.elf
	@# busybox likewise is a ported program built by its own script, and the
	@# kernel embeds it as the system userland. It has to be *the* image under
	@# /bin: /bin/ls and its siblings are symlinks to /bin/busybox, and busybox
	@# picks the applet from argv[0]. A stale copy here would be a userland that
	@# disagrees with the port that is supposed to have built it.
	@cp target/busybox.elf target/user-busybox.elf
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

run: iso# `+rdseed,+rdrand` asks QEMU to present the hardware entropy instructions the
# host CPU has. The kernel probes for them (entropy.rs) and prefers them over the
# timing-jitter fallback, which is measurably weaker; without this flag `make run`
# boots with the fallback and `getrandom` still works, just less well.
	qemu-system-x86_64 \
	    -cdrom $(ISO) \
	    -cpu qemu64,+rdseed,+rdrand \
	    -serial stdio \
	    -display none \
	    -no-reboot

# Boot the ISO built by `make fbterm-run`, which has the ported fbterm embedded.
run-fbterm:
	qemu-system-x86_64 \
	    -cdrom samsara-fbterm.iso \
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
