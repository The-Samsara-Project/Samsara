# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Build rules for the Samsara fbterm port.
#
# These rules deliberately live in the port rather than in the fbterm tree. Two
# reasons, both about reproducibility:
#
#   1. fbterm builds with autotools, and its generated `configure` is checked
#      into upstream. A generated file is not a reproducible input -- it is a
#      build product that happens to be committed, produced by whatever
#      autoconf/automake versions ran last. Replacing the build system with
#      explicit rules makes the input to this build exactly "pinned commit +
#      patches", which is what ports/mlibc/build.sh already guarantees.
#
#   2. Keeping the rules beside the patches that they cooperate with means a
#      reviewer reads the build and the diffs in one directory, and the fbterm
#      checkout stays clean enough that `git diff` shows only our patches.
#
# Invoked by build.sh, which passes FBTERM_SRC / FBTERM_BUILD / SYSROOT / LIBC.

FBTERM_SRC  ?= ../build/fbterm-src
FBTERM_BUILD?= ../build/fbterm-build
SYSROOT     ?= ../build/sysroot
LIBC        ?= $(SYSROOT)/usr/lib/x86_64-pc-samsara

# This file's own directory, for the port-provided UAPI headers in include/.
# Those are the Linux interface definitions fbterm compiles against -- the
# framebuffer and evdev structures, the console keyboard and VT request numbers
# -- supplied here rather than patched into the fbterm tree so that a change to
# one of them is a reviewable edit in the port rather than a hunk in a patch.
PORT := $(dir $(abspath $(lastword $(MAKEFILE_LIST))))

# The freestanding C headers (stdint.h, stdarg.h, ...) live in the mlibc
# checkout rather than the sysroot, so they are addressed relative to SYSROOT
# the same way the link inputs are. build.sh pins that checkout to a commit.
FREESTND := $(SYSROOT)/../mlibc-src/subprojects/freestnd-c-hdrs/x86_64/include

# --- toolchain ----------------------------------------------------------------
#
# These must stay in step with ports/mlibc/samsara.cross-file: the libc was
# compiled with a specific set of target features, and a program compiled
# differently can disagree with it about struct layout, calling convention or
# exception handling. `-fno-exceptions -fno-rtti` are fbterm's own requirement
# (upstream sets them) and are safe here because fbterm uses neither.
#
#   -fno-exceptions / -fno-rtti     fbterm upstream requirement
#   -nostdinc + explicit -isystem   no host headers may leak in; the sysroot is
#                                   the only libc, so a stray <stdio.h> from the
#                                   build host would compile and then fail to link
#   -fno-unwind-tables              Samsara has no unwinder
TARGET  := x86_64-pc-samsara
CC      := clang
CXX     := clang++
# NB: -ffreestanding is deliberately NOT passed for C++, unlike
# ports/mlibc/samsara.cross-file. See the note below; it is the one flag here
# that differs from the libc build, and it is not an oversight.
#
# With -ffreestanding, clang mangles `main` in C++ mode -- it becomes
# _Z4mainiPPc rather than main -- because C++ only guarantees `main` in a
# *hosted* environment, and freestanding says this is not one. mlibc's crt1
# calls an unmangled `main`, so a C++ program built that way links and then
# fails with "undefined symbol: main".
#
# The flag is not needed for the effect people reach for it on a hosted target.
# What actually keeps the build honest here is the bare-metal target triple,
# which tells clang there is no hosted libc to assume, plus -nostdinc so no host
# header can be found and -fno-builtin so no builtin can pull one in. Structure
# layouts and calling conventions are properties of the target, not of
# freestanding-ness, so the libc being built freestanding does not require the
# program to be.
#
# Consequence for the rest of the tree: every C++ program ported to Samsara
# needs this, and the symptom (a missing `main`) looks nothing like its cause.
CXXFLAGS := --target=$(TARGET) \
            -nostdinc -fno-builtin -D_GNU_SOURCE \
            -fno-stack-protector -fno-asynchronous-unwind-tables -fno-unwind-tables \
            -fno-exceptions -fno-rtti \
            -O2 -fPIE \
            -isystem $(SYSROOT)/usr/include \
            -isystem $(FREESTND) \
            -isystem $(PORT)/include \
            -I$(FBTERM_SRC)/src \
            -I$(FBTERM_SRC)/src/lib

# The kernel stages a user image at 0x110000 and there is no dynamic linker, so
# the image base and the `syscall` symbol are fixed by the ABI rather than
# chosen here. `--defsym=syscall=__do_syscall4` is what lets libc's syscall(2)
# reach the kernel's dispatcher; without it every libc call would fail to link.
#
# `__dso_handle` is the static-PIE anchor: a static binary still references it,
# and an unresolved one is a link error.
LDFLAGS := --target=$(TARGET) -ffreestanding -nostdlib -static-pie \
           -fuse-ld=lld \
           -Wl,--image-base=0x110000 \
           -Wl,--defsym=syscall=__do_syscall4 \
           -Wl,--build-id=none
LIBS    := $(LIBC)/crt1.o $(LIBC)/libc.a

# --- sources -------------------------------------------------------------------
#
# Upstream's own list (src/Makefile.am and src/lib/Makefile.am) minus the
# translation units this port removes. Each omission has a patch removing it
# too, so the two never drift apart; see README.md for why each is gone.
LIB_SOURCES := \
	src/lib/io.cpp \
	src/lib/shell.cpp \
	src/lib/vterm.cpp \
	src/lib/vterm_action.cpp \
	src/lib/vterm_states.cpp \
	src/lib/wcwidth.cpp \
	src/lib/charsetmap.cpp

MAIN_SOURCES := \
	src/fbconfig.cpp \
	src/fbio.cpp \
	src/fbshell.cpp \
	src/fbshellman.cpp \
	src/fbterm.cpp \
	src/font.cpp \
	src/input.cpp \
	src/improxy.cpp \
	src/mouse.cpp \
	src/screen.cpp \
	src/screen_render.cpp \
	src/fbdev.cpp

# Port-supplied replacements for libc functions fbterm needs and mlibc does not
# have. These are built from the port rather than patched into fbterm, so the
# fbterm tree stays "upstream plus patches" and the replacements are reviewable
# on their own.
#
# Defined *before* SOURCES, which matters: SOURCES uses `:=`, and that expands
# immediately, so a PORT_SOURCES defined afterwards would be empty here and the
# two objects would silently drop out of the link. The failure would surface as
# an undefined `forkpty` at link time rather than as an obvious mistake here.
PORT_SOURCES := \
	support/forkpty.cpp \
	support/getopt_long.cpp \
	support/new_delete.cpp

SOURCES := $(LIB_SOURCES) $(MAIN_SOURCES) $(PORT_SOURCES)
OBJECTS := $(addprefix $(FBTERM_BUILD)/,$(SOURCES:.cpp=.o))
DEPS    := $(OBJECTS:.o=.d)

# Compile lines are hidden unless V=1. A full build is ~20 clang invocations
# with paths long enough to wrap several times, which buries the handful of
# lines that actually carry information (errors, and the final fingerprint).
ifeq ($(V),1)
Q :=
else
Q := @
endif

.PHONY: all clean
all: $(FBTERM_BUILD)/fbterm.elf

$(FBTERM_BUILD)/fbterm.elf: $(OBJECTS) $(FBTERM_BUILD)/dso.o
	@mkdir -p $(dir $@)
	$(Q)$(CXX) $(LDFLAGS) -o $@ $^ $(LIBS)
	@echo "fbterm: built $@"

# Object rule. -MMD -MP emits a .d alongside each .o so a header edit rebuilds
# only what includes it; without that a stale object silently survives a change
# to, say, font.h.
$(FBTERM_BUILD)/%.o: $(FBTERM_SRC)/%.cpp
	@mkdir -p $(dir $@)
	$(Q)$(CXX) $(CXXFLAGS) -MMD -MP -c -o $@ $<

# Port-supplied sources live in the port, not in the fbterm tree, so they need
# their own pattern rule: the generic one above would look for them under
# $(FBTERM_SRC).
$(FBTERM_BUILD)/%.o: $(PORT)/%.cpp
	@mkdir -p $(dir $@)
	$(Q)$(CXX) $(CXXFLAGS) -MMD -MP -I$(PORT)/include -c -o $@ $<

# The static-PIE anchor described above.
$(FBTERM_BUILD)/dso.o:
	@mkdir -p $(dir $@)
	@printf 'void *__dso_handle = 0;\n' > $(FBTERM_BUILD)/dso.c
	$(Q)$(CC) $(CXXFLAGS) -c -o $@ $(FBTERM_BUILD)/dso.c

clean:
	rm -rf $(FBTERM_BUILD)

-include $(DEPS)
