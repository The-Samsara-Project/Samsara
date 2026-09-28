#!/usr/bin/env bash
#
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Reproducible build of fbterm against the Samsara kernel ABI.
#
# This is the same method ports/mlibc/build.sh uses, deliberately: pin one
# upstream commit, clone it into build/ (gitignored), apply patches from
# patches/, build with the port's own rules, and print a fingerprint. No fbterm
# source is vendored, and no autotools run is involved -- the generated
# `configure` is not a reproducible input, so the port supplies its own rules.
#
# Everything this port needs lives in this directory:
#   build.sh              this script
#   Makefile              the build rules (replaces fbterm's autotools)
#   patches/              patches applied on top of the pinned fbterm commit
#   README.md             what was changed and why
#
# Deterministic within a pinned commit: for the same fbterm SHA the output
# binary is a function of ports/fbterm/ and the sysroot alone.
#
#   ./ports/fbterm/build.sh
#
# Optional knobs:
#   SAMSARA_FBTERM_SRC   point at an existing fbterm checkout to reuse instead
#                       of cloning (still verified against the pinned SHA).
#
# Output:
#   build/fbterm-build/fbterm.elf
#
# Host requirements: git, clang/clang++ with --target=x86_64-pc-samsara
# support, lld, and a built mlibc sysroot (run ports/mlibc/build.sh first --
# the link step needs libc.a and crt1.o).

set -euo pipefail

# The only commit this port is ever built from. fbterm has been unmaintained for
# over a decade, so "latest" is not a moving target worth tracking; pinning the
# tip is also pinning the last state anyone ever tested.
POINT_OF_TRUTH=419c6be55413a7069fbcc66883d58caa94ba3933
FBTERM_REPO="https://github.com/sfzhi/fbterm.git"

PORT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$PORT/../.." && pwd)"
FBTERM_SRC="${SAMSARA_FBTERM_SRC:-$ROOT/build/fbterm-src}"
FBTERM_BUILD="$ROOT/build/fbterm-build"
SYSROOT="$ROOT/build/sysroot"
LIBC="$SYSROOT/usr/lib/x86_64-pc-samsara"

say() { printf '\033[1;34m[fbterm]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[fbterm]\033[0m %s\n' "$*" >&2; exit 1; }

# --- 0. Preconditions --------------------------------------------------------
# Checked before anything is cloned so a missing sysroot fails in a second
# rather than after a network round trip.
[ -f "$LIBC/libc.a" ] || die "no mlibc sysroot at $LIBC -- run ports/mlibc/build.sh first"
[ -f "$LIBC/crt1.o" ] || die "no crt1.o at $LIBC -- run ports/mlibc/build.sh first"

# --- 1. Reconcile the pinned clone --------------------------------------------
if [ ! -d "$FBTERM_SRC/.git" ]; then
  if [ -n "${SAMSARA_FBTERM_SRC:-}" ]; then
    die "SAMSARA_FBTERM_SRC=$FBTERM_SRC is not a git checkout"
  fi
  say "cloning fbterm @ ${POINT_OF_TRUTH:0:12}"
  rm -rf "$FBTERM_SRC"
  git clone --quiet "$FBTERM_REPO" "$FBTERM_SRC"
fi

git -C "$FBTERM_SRC" fetch --quiet "$FBTERM_REPO" "$POINT_OF_TRUTH" \
  || git -C "$FBTERM_SRC" fetch --quiet origin
if [ "$(git -C "$FBTERM_SRC" rev-parse HEAD)" != "$POINT_OF_TRUTH" ]; then
  say "checking out pinned commit"
  git -C "$FBTERM_SRC" checkout --quiet "$POINT_OF_TRUTH"
fi

# Roll back residue from a previous run so re-running is idempotent, then refuse
# to build a tree that is not the pinned commit plus our patches. Silently
# building a modified tree is how a port stops being reproducible.
if ! git -C "$FBTERM_SRC" diff --quiet HEAD; then
  say "restoring pristine tree (state left by a previous run)"
  git -C "$FBTERM_SRC" reset --hard "$POINT_OF_TRUTH"
  git -C "$FBTERM_SRC" clean -qfd
fi
dirty="$(git -C "$FBTERM_SRC" status --porcelain || true)"
if [ -n "$dirty" ]; then
  die "fbterm tree is not byte-identical to the pinned commit — refusing to build:
$dirty"
fi

# --- 2. Apply patches (idempotent) --------------------------------------------
for patch in "$PORT"/patches/*.patch; do
  [ -e "$patch" ] || continue
  if git -C "$FBTERM_SRC" apply --check "$patch" 2>/dev/null; then
    say "applying $(basename "$patch")"
    git -C "$FBTERM_SRC" apply "$patch"
  elif git -C "$FBTERM_SRC" apply --reverse --check "$patch" 2>/dev/null; then
    say "skipping already-applied $(basename "$patch")"
  else
    die "$(basename "$patch") does not apply cleanly to ${POINT_OF_TRUTH:0:12}
The pinned commit and this port's patches have drifted apart."
  fi
done

# --- 3. Build ------------------------------------------------------------------
# The build rules live in the port, not in the fbterm tree, so the tree stays
# byte-identical to "pinned commit + patches" and a reviewer can read the rules
# next to the patches that they work with.
rm -rf "$FBTERM_BUILD"
say "building (freestanding clang, static)"
make -f "$PORT/Makefile" \
  FBTERM_SRC="$FBTERM_SRC" \
  FBTERM_BUILD="$FBTERM_BUILD" \
  SYSROOT="$SYSROOT" \
  LIBC="$LIBC" \
  --no-print-directory

# --- 4. Report -------------------------------------------------------------------
say "done; pinned ${POINT_OF_TRUTH} at $FBTERM_SRC"
ls -l "$FBTERM_BUILD/fbterm.elf" | sed 's/^/  /'
say "reproducibility fingerprint:"
sha256sum "$FBTERM_BUILD/fbterm.elf" | sed 's/^/  /'
