mlibc port for Samsara
======================

This directory is a self-contained, reproducible port of
[mlibc](https://github.com/managarm/mlibc) against the Samsara kernel ABI.
Everything needed to produce the sysroot lives here; no mlibc source is
vendored.

Layout
------

    build.sh                 entry point; see the header comment for the
                             exact pipeline and optional knobs
    samsara.cross-file       meson cross file: freestanding clang toolchain
                             targeting x86_64-pc-samsara, plus the feature
                             macros the build needs (see comments in-file)
    patches/                 0001-*.patch applied on top of the pinned commit
    sysdeps/samsara/         the Samsara sysdep tag: abi-bits headers and the
                             syscall glue (syscall.cpp, sysdeps.cpp, entry.cpp)

Building
--------

Host requirements: git, ninja, [meson] >= 1.3.0, clang/clang++ (with
`--target=x86_64-pc-samsara` support), llvm-ar and lld.

    ./ports/mlibc/build.sh

That clones the pinned mlibc commit into `build/mlibc-src`, applies the port's
patches, installs the sysdeps tag, configures a static build and installs the
result into `build/sysroot`:

    build/sysroot/usr/lib/x86_64-pc-samsara/{libc.a,crt1.o,libdl.a,...}
    build/sysroot/usr/include/**

The script is idempotent and reproducible: for the same pinned commit the
output is a function of this directory only, and the final `sha256sum` of
`libc.a`/`crt1.o` lets you verify two builds agree.

Rebuilding the port against a newer mlibc
-----------------------------------------

1. Bump `POINT_OF_TRUTH` in `build.sh` to the new commit SHA.
2. If any of our changes no longer apply cleanly, regenerate the affected
   patch: `git diff` in your checkout against the pin, save under `patches/`.
3. Keep the sysdeps tag and cross-file in sync with whatever the new tree
   requires, then update this README if the ABI surface changed.

Notes on the current configuration
----------------------------------

* `sysdep_supported_options` enables only `posix` (like the upstream `demo`
  port). `glibc` is deliberately disabled: it would pull in
  `strverscmp`/`versionsort` and friends that Samsara does not implement.
* `-Dcpp_std=gnu++23` (strict `c++23` sets `__STRICT_ANSI__`, which turns off
  mlibc's default-source feature set and breaks `<netdb.h>`'s `h_errno`,
  `HOST_*`, `strlcpy`, ...).
* `-D_GNU_SOURCE` in the cross-file makes `Dl_info`/`dladdr()` available for
  `options/posix/generic/dlfcn.cpp` without enabling the glibc option.

[meson]: https://mesonbuild.com/