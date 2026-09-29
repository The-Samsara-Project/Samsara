#!/usr/bin/env bash
#
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Harsh Nikarsa
#
# Reproducible build of mlibc against the Samsara kernel ABI.
#
# Everything this port needs lives in this directory:
#   build.sh                  this script
#   samsara.cross-file        the freestanding clang cross/meson config
#   patches/                  patches applied on top of the pinned mlibc commit
#   sysdeps/samsara/          the Samsara sysdep tag (ABI bits, syscall glue)
#
# No mlibc source is vendored. The script pins one upstream commit, clones it
# into build/ (gitignored), applies the patches, drops the sysdeps tag into the
# tree, then builds + installs a static libc with the freestanding clang
# toolchain declared in samsara.cross-file.
#
# Deterministic within a pinned commit: for the same mlibc SHA the output
# sysroot is a function of ports/mlibc/ only (see the sha256sum printed at the
# end for a quick reproducibility check).
#
#   ./ports/mlibc/build.sh
#
# Optional knobs:
#   SAMSARA_MLIBC_SRC   point at an existing mlibc checkout to reuse instead of
#                       cloning (still verified against the pinned SHA).
#
# Outputs (installed sysroot):
#   build/sysroot/usr/lib/x86_64-pc-samsara/{libc.a,crt1.o,...}
#   build/sysroot/usr/include/*  (mlibc headers + abi-bits)
#
# Host requirements: git, ninja, meson (>= 1.3.0), clang/clang++ with
# --target=x86_64-pc-samsara support, llvm-ar, lld.

set -euo pipefail

POINT_OF_TRUTH=3bf6b90851b4390ebccf068a316459aa7702d95e
MLIBC_REPO="https://github.com/managarm/mlibc.git"

PORT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$PORT/../.." && pwd)"
MLIBC_SRC="${SAMSARA_MLIBC_SRC:-$ROOT/build/mlibc-src}"
MLIBC_BUILD="$ROOT/build/mlibc-build"
SYSROOT="$ROOT/build/sysroot"

say() { printf '\033[1;34m[mlibc]\033[0m %s\n' "$*"; }
die() { printf '\033[1;31m[mlibc]\033[0m %s\n' "$*" >&2; exit 1; }

# --- 0. Reconcile the pinned clone -----------------------------------------
if [ ! -d "$MLIBC_SRC/.git" ]; then
  if [ -n "${SAMSARA_MLIBC_SRC:-}" ]; then
    die "SAMSARA_MLIBC_SRC=$MLIBC_SRC is not a git checkout"
  fi
  say "cloning mlibc @ ${POINT_OF_TRUTH:0:12} (this can take a couple of minutes)"
  rm -rf "$MLIBC_SRC"
  git clone --quiet "$MLIBC_REPO" "$MLIBC_SRC"
fi

git -C "$MLIBC_SRC" fetch --quiet "$MLIBC_REPO" "$POINT_OF_TRUTH" \
  || git -C "$MLIBC_SRC" fetch --quiet origin
if [ "$(git -C "$MLIBC_SRC" rev-parse HEAD)" != "$POINT_OF_TRUTH" ]; then
  say "checking out pinned commit"
  git -C "$MLIBC_SRC" checkout --quiet "$POINT_OF_TRUTH"
fi

# Roll back any residue from a previous run (idempotent re-runs), then fail
# on anything the port doesn't own. The copied sysdeps/ and an applied patch
# are expected after a prior build; anything else means the tree was tampered
# with and would silently produce a different libc.
if ! git -C "$MLIBC_SRC" diff --quiet HEAD; then
  say "restoring pristine tree (state left by a previous run)"
  git -C "$MLIBC_SRC" reset --hard "$POINT_OF_TRUTH"
fi
dirty="$(git -C "$MLIBC_SRC" status --porcelain | grep -v '^?? sysdeps/samsara/$' || true)"
if [ -n "$dirty" ]; then
  die "mlibc tree is not byte-identical to the pinned commit — refusing to build:
$dirty"
fi

# --- 1. Apply Samsara patches (idempotent) ---------------------------------
for patch in "$PORT"/patches/*.patch; do
  if git -C "$MLIBC_SRC" apply --check "$patch" 2>/dev/null; then
    say "applying $(basename "$patch")"
    git -C "$MLIBC_SRC" apply "$patch"
  else
    say "skipping already-applied $(basename "$patch")"
  fi
done

# --- 2. Copy the Samsara sysdeps into the tree ------------------------------
say "syncing sysdeps/samsara"
rm -rf "$MLIBC_SRC/sysdeps/samsara"
cp -r "$PORT/sysdeps/samsara" "$MLIBC_SRC/sysdeps/samsara"

# --- 3. Configure meson ------------------------------------------------------
rm -rf "$MLIBC_BUILD" "$SYSROOT"
say "meson setup (freestanding clang, static lib)"

# Stamp the build into the libc as SAMSARA_BUILD, which the port's `uname`
# reports in the version field. A bug report that pastes `uname` output then
# identifies the build it came from, which is the whole reason that field exists.
# ISO 8601 UTC, so it sorts and parses.
#
# The date is the *pinned commit's*, not the wall clock's, and that is not a
# detail. The stamp is compiled into `libc.a`, so a clock reading makes the
# library different on every build and the fingerprint printed at the end of this
# script different with it -- which makes the fingerprint useless for the one
# thing a fingerprint is for. The claim this script makes is that a fresh clone
# builds the same bytes, and a timestamp in the artifact is exactly the kind of
# thing that quietly makes that false.
#
# The commit's own date is also the more useful half of the answer to "which build
# is this?". "When did someone run build.sh" is a property of the machine;
# "which mlibc" is a property of the bug. The commit is what is pinned, so the
# date follows from it and needs no separate bookkeeping.
if ! SAMSARA_BUILD="$(git -C "$MLIBC_SRC" log -1 --format=%cI "$POINT_OF_TRUTH" 2>/dev/null)"; then
  die "cannot read the commit date of $POINT_OF_TRUTH"
fi
# `%cI` is an offset-bearing ISO 8601 date ("2026-09-13T17:43:55+02:00"). Trim it
# to the UTC `Z` form so it matches what a reader expects from a build stamp.
SAMSARA_BUILD="$(date -u -d "$SAMSARA_BUILD" +%Y-%m-%dT%H:%M:%SZ)"
say "build stamp: $SAMSARA_BUILD (from mlibc ${POINT_OF_TRUTH:0:12})"

# The stamp has to survive two layers of shell and meson's argument splitting,
# and a `:` in a `-D` value is read as a meson key separator, so the value is
# passed through the environment and the port's meson.build reads it from there.
# `yy-mm-ddTHH:MM:SSZ` stays unambiguous; the colons are inside a single-quoted
# meson string.
SAMSARA_BUILD="$SAMSARA_BUILD" meson setup "$MLIBC_BUILD" "$MLIBC_SRC" \
  --cross-file="$PORT/samsara.cross-file" \
  --prefix=/usr \
  --libdir=lib/x86_64-pc-samsara \
  -Ddefault_library=static \
  -Dlibgcc_dependency=false \
  -Duse_freestnd_hdrs=enabled \
  -Dbuild_tests=false \
  -Dheaders_only=false \
  -Dwerror=false \
  -Dcpp_std=gnu++23

# --- 4. Build ---------------------------------------------------------------
say "ninja"
ninja -C "$MLIBC_BUILD"

# --- 5. Install a sysroot ----------------------------------------------------
say "installing sysroot"
DESTDIR="$SYSROOT" ninja -C "$MLIBC_BUILD" install

# --- 6. Report ---------------------------------------------------------------
say "done; pinned ${POINT_OF_TRUTH} at $MLIBC_SRC"
ls -l "$SYSROOT/usr/lib/x86_64-pc-samsara/" | sed 's/^/  /'
say "reproducibility fingerprints:"
sha256sum \
  "$SYSROOT/usr/lib/x86_64-pc-samsara/libc.a" \
  "$SYSROOT/usr/lib/x86_64-pc-samsara/crt1.o" | sed 's/^/  /'