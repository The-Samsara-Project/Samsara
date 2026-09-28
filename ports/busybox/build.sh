#!/usr/bin/env bash
#
# SPDX-License-Identifier: GPL-2.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Reproducible build of busybox against the Samsara kernel ABI.
#
# Same method as ports/mlibc/build.sh and ports/fbterm/build.sh, deliberately:
# pin one upstream commit, clone it into build/ (gitignored), apply patches from
# patches/, build with the port's own rules, print a fingerprint. No busybox
# source is vendored.
#
# Everything this port needs lives in this directory:
#   build.sh              this script
#   config                the kconfig fragment: which applets and features, and
#                         why each group of omissions is omitted
#   patches/              patches applied on top of the pinned commit
#   include/              headers this libc lacks, supplied by the port
#   support/dso.c         the __dso_handle anchor
#   toolchain/cc          the compiler wrapper
#   toolchain/merge-config  merges config onto the config kconfig generates
#
# Deterministic within a pinned commit: for the same busybox SHA the output
# binary is a function of ports/busybox/ and the sysroot alone.
#
#   ./ports/busybox/build.sh
#
# Optional knobs:
#   SAMSARA_BUSYBOX_SRC   point at an existing busybox checkout to reuse instead
#                         of cloning (still verified against the pinned SHA).
#
# Output:
#   build/busybox-src/busybox
#   target/busybox.elf
#
# Host requirements: git, clang with --target=x86_64-pc-samsara support, lld, and
# a built mlibc sysroot (run ports/mlibc/build.sh first -- the link step needs
# libc.a and crt1.o).

set -euo pipefail

# The only commit this port is ever built from.
POINT_OF_TRUTH=371fe9f71d445d18be28c82a2a6d82115c8af19d
BUSYBOX_REPO="https://github.com/mirror/busybox.git"

PORT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$PORT/../.." && pwd)"
BUSYBOX_SRC="${SAMSARA_BUSYBOX_SRC:-$ROOT/build/busybox-src}"
SYSROOT="$ROOT/build/sysroot"
LIBC="$SYSROOT/usr/lib/x86_64-pc-samsara"
LIBMLIBC="$ROOT/build/mlibc-src/subprojects/freestnd-c-hdrs/x86_64/include"
TARGET="$ROOT/target"
ANCHOR="$ROOT/build/busybox-anchor"

say() { printf '\033[1;34m[busybox]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[busybox]\033[0m %s\n' "$*" >&2; exit 1; }

# --- 0. Preconditions --------------------------------------------------------
# Before anything is cloned, so a missing sysroot fails in a second rather than
# after a network round trip.
[ -f "$LIBC/libc.a" ] || die "no mlibc sysroot at $LIBC -- run ports/mlibc/build.sh first"
[ -f "$LIBC/crt1.o" ] || die "no crt1.o at $LIBC -- run ports/mlibc/build.sh first"
[ -d "$LIBMLIBC" ] || die "no freestnd-c-hdrs at $LIBMLIBC -- run ports/mlibc/build.sh first"

# --- 1. Reconcile the pinned clone -------------------------------------------
if [ ! -d "$BUSYBOX_SRC/.git" ]; then
  if [ -n "${SAMSARA_BUSYBOX_SRC:-}" ]; then
    die "SAMSARA_BUSYBOX_SRC=$BUSYBOX_SRC is not a git checkout"
  fi
  say "cloning busybox @ ${POINT_OF_TRUTH:0:12}"
  rm -rf "$BUSYBOX_SRC"
  git clone --quiet "$BUSYBOX_REPO" "$BUSYBOX_SRC"
fi

git -C "$BUSYBOX_SRC" fetch --quiet "$BUSYBOX_REPO" "$POINT_OF_TRUTH" \
  || git -C "$BUSYBOX_SRC" fetch --quiet origin
if [ "$(git -C "$BUSYBOX_SRC" rev-parse HEAD)" != "$POINT_OF_TRUTH" ]; then
  say "checking out pinned commit"
  git -C "$BUSYBOX_SRC" checkout --quiet "$POINT_OF_TRUTH"
fi

# Roll back residue from a previous run so re-running is idempotent, then refuse
# to build a tree that is not the pinned commit plus our patches. Silently
# building a modified tree is how a port stops being reproducible.
if ! git -C "$BUSYBOX_SRC" diff --quiet HEAD; then
  say "restoring pristine tree (state left by a previous run)"
  git -C "$BUSYBOX_SRC" reset --hard "$POINT_OF_TRUTH"
  git -C "$BUSYBOX_SRC" clean -qfd
fi

# --- 2. Apply patches (idempotent) -------------------------------------------
for patch in "$PORT"/patches/*.patch; do
  [ -e "$patch" ] || continue
  if git -C "$BUSYBOX_SRC" apply --check "$patch" 2>/dev/null; then
    say "applying $(basename "$patch")"
    git -C "$BUSYBOX_SRC" apply "$patch"
  elif git -C "$BUSYBOX_SRC" apply --reverse --check "$patch" 2>/dev/null; then
    say "skipping already-applied $(basename "$patch")"
  else
    die "$(basename "$patch") does not apply cleanly to ${POINT_OF_TRUTH:0:12}
The pinned commit and this port's patches have drifted apart."
  fi
done

# Patches are allowed to add tracked files, so check for untracked residue only
# after they are in. build/ products (.config, *.o, the binary) are what a build
# leaves behind and are not drift.
untracked="$(git -C "$BUSYBOX_SRC" ls-files --others --exclude-standard || true)"
if [ -n "$untracked" ]; then
  die "busybox tree has untracked files — refusing to build:
$untracked"
fi

# --- 3. Configure ------------------------------------------------------------
# Reproducibility, via the switch busybox already provides, and it has to be set
# here rather than beside the build: kconfig stamps include/autoconf.h with the
# wall clock, and libbb/messages.c bakes that stamp into the version banner, so two
# builds of the same pinned commit differ in two bytes of
# "BusyBox v1.37.0.git (2026-09-28 09:30:12 EET)". A fingerprint that changes every
# run is not a fingerprint, it is a clock.
#
# KCONFIG_NOTIMESTAMP is busybox's own opt-out -- scripts/kconfig/confdata.c reads
# it and writes an empty AUTOCONF_TIMESTAMP -- so the banner becomes
# "BusyBox v1.37.0.git" and the build is a function of the pinned commit, this
# directory, and the sysroot. The banner loses the build time, which is the right
# thing to lose: a time is not a property of the program.
export KCONFIG_NOTIMESTAMP=1

# Three steps, and the order matters.
#
#   allnoconfig   a complete config with every symbol off
#   merge         the port's fragment on top of it
#   oldconfig     kconfig resolves the dependencies, and tells us by name about
#                 any symbol in the fragment that does not exist
#
# busybox's KCONFIG_ALLCONFIG cannot do this in one step; see the comment at the
# top of toolchain/merge-config.
cd "$BUSYBOX_SRC"
say "configuring"
make --no-print-directory allnoconfig >/dev/null

"$PORT/toolchain/merge-config" "$PORT/config" .config

# Anything this port puts in the config that is a fact about *this machine*
# rather than about the port goes here, never in config. A checked-in config
# with a build path in it cannot be moved to another checkout.
{
  echo "# Generated by ports/busybox/build.sh -- machine-specific, not portable."
  echo "CONFIG_EXTRA_LDFLAGS=\"-nostdlib -static -L$LIBC -Wl,--defsym=syscall=__do_syscall4 -Wl,--build-id=none\""
} > build-local.config
"$PORT/toolchain/merge-config" build-local.config .config

# oldconfig is the only thing that knows the real symbol table, so it is also the
# only place a mistake in the fragment can be caught. Its warnings are read, not
# skimmed, and each names a different kind of mistake:
#
#   nonexistent symbol      an option was renamed or never existed
#   reassign symbol         two fragments disagreed about one symbol
#   symbol value invalid    the value has the wrong *shape* -- `=y` for an int
#                           option, say.
#
# It is fed blank answers rather than /dev/null, and that is not a convenience.
# oldconfig is an interactive program: conf_askvalue falls through to fgets() for
# any symbol that has no value yet, and hits exit(1) at end of file. Symbols with
# no value are exactly the ones the fragment brought into existence -- allnoconfig
# writes a string option as `""` when its parent is off, so turning the parent on
# leaves the child looking unanswered. A blank line means "take the default",
# which is the ordinary meaning of running oldconfig on a config that is already
# complete; /dev/null means "die on the first question", which is what it did.
#
# Newlines are written to a file rather than piped, because `yes |` leaves the
# writer killed by SIGPIPE the moment make stops reading, and `set -o pipefail`
# would turn that into a build failure that looks like make's.
answers=$(mktemp)
conf_log=$(mktemp)
trap 'rm -f "$answers" "$conf_log"' EXIT
printf '\n%.0s' $(seq 1 8192) >"$answers"

make --no-print-directory oldconfig <"$answers" >"$conf_log" 2>&1
if grep -qE "nonexistent symbol|reassign symbol|symbol value .* invalid" "$conf_log"; then
  grep -E "nonexistent symbol|reassign symbol|symbol value .* invalid" "$conf_log" >&2
  die "ports/busybox/config does not match this busybox's symbol table"
fi
rm -f "$answers" "$conf_log"
trap - EXIT

applets=$(grep -cE "^CONFIG_[A-Z_0-9]+=y$" .config)
say "configured: $applets options enabled"

# --- 4. Objects that are not libraries ---------------------------------------
# The wrapper reads its paths from the environment rather than having them baked
# in, so that the same file works on any host and nothing has to be regenerated
# when the tree moves. Exported here rather than beside each use, because busybox
# invokes $(CC) from every one of its own recipes, in sub-makes, and each of those
# needs to find them.
export SAMSARA_SYSROOT="$SYSROOT"
export SAMSARA_FREESTND="$LIBMLIBC"
export SAMSARA_PORT_INCLUDE="$PORT/include"

# Two of these are named rather than compiled, two come from support/. Each is
# here because it has to be on the final link line and cannot be reached by -l:
#
#   crt1.o   the ELF entry point, which nothing references
#   dso.o    __dso_handle, which nothing defines. See support/dso.c for why the
#            value must be zero rather than merely present.
#   mntent.o the mount table, which is always empty. See support/mntent.c.
mkdir -p "$ANCHOR"
"$PORT/toolchain/cc" -ffreestanding -nostdinc -c \
  -o "$ANCHOR/dso.o" "$PORT/support/dso.c"
"$PORT/toolchain/cc" -ffreestanding -nostdinc -c \
  -o "$ANCHOR/mntent.o" "$PORT/support/mntent.c"

# --- 5. Build ------------------------------------------------------------------
say "building"
make --no-print-directory -j"$(nproc)" \
  CC="$PORT/toolchain/cc" \
  LD=ld.lld \
  AR=llvm-ar \
  NM=llvm-nm \
  SKIP_STRIP=n \
  CONFIG_SAMSARA_LINK_OBJS="$LIBC/crt1.o $ANCHOR/dso.o"

[ -f busybox ] || die "no busybox binary was produced"

# --- 6. Report -----------------------------------------------------------------
mkdir -p "$TARGET"
cp busybox "$TARGET/busybox.elf"

say "done; pinned ${POINT_OF_TRUTH} at $BUSYBOX_SRC"
ls -l "$TARGET/busybox.elf" | sed 's/^/  /'

# How many applets actually made it in.
#
# Read from include/applet_tables.h, which is generated by busybox's own applet
# generator from the resolved config, and not from .config. The two differ
# whenever a symbol's dependencies did not hold -- an option that was asked for
# and silently dropped -- and the table is what the binary dispatches on, so the
# table is the number worth reporting. NUM_APPLETS is the generator's own count;
# counting the names as well would be a second opinion about the same file.
#
# The names are listed too, because a configuration is meant to be read, and the
# cheapest way to notice that a group of applets is missing is to see it here.
num_applets=$(sed -n 's/^#define NUM_APPLETS \([0-9]*\).*/\1/p' include/applet_tables.h)
[ -n "$num_applets" ] || die "include/applet_tables.h has no NUM_APPLETS -- applet generation failed"

say "applets: $num_applets"
# busybox writes one string literal per applet, each suffixed with an explicit \0
# and each on its own line, so the names come out by stripping both. A literal
# containing a real backslash is not an applet name and does not occur.
applet_names() {
  sed -n '/^const char applet_names/,/^$/p' include/applet_tables.h |
    sed -n 's/^"\(.*\)" "\\0"$/\1/p'
}
applet_names | paste -sd' ' - | fold -s -w 74 | sed 's/^/  /'
listed=$(applet_names | wc -l)
[ "$listed" = "$num_applets" ] ||
  die "applet table lists $listed names but claims NUM_APPLETS $num_applets"
say "reproducibility fingerprint:"
sha256sum "$TARGET/busybox.elf" | sed 's/^/  /'
