// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

//! Entropy for `getrandom(2)`.
//!
//! This is deliberately the most cautious thing in the tree, because the
//! failure mode of getting it wrong is silent and severe: a program that asks
//! for random bytes and gets predictable ones builds session tokens, temp-file
//! names and hash seeds on them, and nothing reports an error. A stub that
//! returns `ENOSYS` is strictly safer than a generator that is not actually
//! random, so that is what this falls back to.
//!
//! Sources, in order of preference:
//!
//!   1. **RDSEED / RDRAND.** A hardware entropy generator with its own health
//!      test. This is the only source here that is a real entropy source rather
//!      than a physical measurement, and it is what a machine with it should
//!      use. Availability comes from CPUID, never from assuming.
//!   2. **HPET jitter.** The HPET is a free-running counter on its own clock
//!      domain, sampled around a variable busy-wait. The spread comes from
//!      interrupt and bus timing rather than from the instruction stream, which
//!      is what makes it more than a PRNG with a constant seed. It is *not*
//!      cryptographically strong and is documented as such.
//!
//! The pool is hashed with SipHash-1-3 between fills, so callers never see raw
//! counter values and successive requests do not correlate.

use core::sync::atomic::{AtomicU64, Ordering};

/// Whether the CPU has a hardware entropy instruction we may use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    /// `RDSEED` (16 bytes, own health test).
    RdSeed,
    /// `RDRAND` (64 bits, no health test of its own).
    RdRand,
    /// Measured jitter from a free-running counter. Not a true entropy source.
    Jitter,
    /// Nothing usable; callers get `ENOSYS`.
    None,
}

static SOURCE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0xff);
static POOL: AtomicU64 = AtomicU64::new(0);
static POOL_SEED: AtomicU64 = AtomicU64::new(0);

/// `cpuid(leaf, subleaf)` -> `(eax, ebx, ecx, edx)`.
///
/// `core::arch::__cpuid_count` rather than hand-written asm: `ebx` is the PIC
/// register on x86-64 and LLVM refuses to name it as an asm operand, so the
/// intrinsic's internal push/pop is the way to read it without fighting the
/// compiler.
fn cpuid(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    // SAFETY: `cpuid` is side-effect free apart from its outputs, and the
    // intrinsic handles the `rbx` save/restore.
    let r = unsafe { core::arch::x86_64::__cpuid_count(leaf, subleaf) };
    (r.eax, r.ebx, r.ecx, r.edx)
}

/// Probe once for the best available source.
fn source() -> Source {
    let cached = SOURCE.load(Ordering::Relaxed);
    if cached != 0xff {
        return match cached {
            0 => Source::RdSeed,
            1 => Source::RdRand,
            2 => Source::Jitter,
            _ => Source::None,
        };
    }
    // CPUID leaf 0 gives the highest supported leaf; asking for a leaf the CPU
    // does not implement returns whatever it *does* implement, so the bound has
    // to be checked before trusting a feature bit from a high leaf.
    let (max_leaf, _, _, _) = cpuid(0, 0);
    let mut found = Source::None;
    if max_leaf >= 7 {
        // RDSEED: leaf 7, subleaf 0, EBX bit 18. Preferred: it has its own
        // health test, so a starved or failing generator says so instead of
        // returning a biased digit.
        let (_, ebx, _, _) = cpuid(7, 0);
        if ebx & (1 << 18) != 0 {
            found = Source::RdSeed;
        }
    }
    if found == Source::None && max_leaf >= 1 {
        // RDRAND: leaf 1, ECX bit 30. No health test of its own, so it is only
        // used when RDSEED is absent.
        let (_, _, ecx, _) = cpuid(1, 0);
        if ecx & (1 << 30) != 0 {
            found = Source::RdRand;
        }
    }
    if found == Source::None && crate::io::hpet::available() {
        found = Source::Jitter;
    }
    SOURCE.store(
        match found {
            Source::RdSeed => 0,
            Source::RdRand => 1,
            Source::Jitter => 2,
            Source::None => 3,
        },
        Ordering::Release,
    );
    if found == Source::None {
        crate::log::kwarn!("entropy: no hardware source and no HPET; getrandom will fail");
    }
    found
}

/// Draw 64 bits, or `None` if this attempt failed.
///
/// An entropy instruction can legitimately report failure -- RDSEED when its
/// health test rejects the raw digit, RDRAND when a core is starved -- so a
/// single call is not guaranteed to produce anything. The caller retries.
fn draw() -> Option<u64> {
    match source() {
        Source::RdSeed => {
            let (lo, hi): (u64, u64);
            let ok: u32;
            // SAFETY: `rdseed` writes its two destinations and sets the carry
            // flag on failure. It is a plain read with no memory effects, and
            // it is only reached once CPUID has confirmed the instruction
            // exists. `setc` needs an 8-bit destination, hence `ok: u8`.
            unsafe {
                core::arch::asm!(
                    "rdseed {lo}",
                    "rdseed {hi}",
                    "setc {ok:l}",
                    lo = out(reg) lo,
                    hi = out(reg) hi,
                    ok = lateout(reg) ok,
                    options(nomem, nostack, preserves_flags),
                );
            }
            if ok != 0 {
                Some(lo ^ hi.rotate_left(32))
            } else {
                None
            }
        }
        Source::RdRand => {
            let v: u64;
            let ok: u32;
            // SAFETY: as above, gated on CPUID reporting RDRAND.
            unsafe {
                core::arch::asm!(
                    "rdrand {v}",
                    "setc {ok:l}",
                    v = out(reg) v,
                    ok = lateout(reg) ok,
                    options(nomem, nostack, preserves_flags),
                );
            }
            if ok != 0 {
                Some(v)
            } else {
                None
            }
        }
        Source::Jitter => Some(jitter()),
        Source::None => None,
    }
}

/// One jitter sample.
///
/// The value that carries the uncertainty is the *interval* the sample took,
/// not the counter's own reading: a counter is a clock, and clocks are
/// predictable to anyone able to time one. What is not predictable to an
/// outsider is exactly when an interrupt landed and how much work the CPU
/// actually retired during the wait.
///
/// Several sources are mixed rather than one, because any single one of them
/// repeats: a free-running counter sampled around a short spin returns the same
/// interval every time under emulation, which is how an earlier version of this
/// produced identical bytes on consecutive calls.
fn jitter() -> u64 {
    /// Incremented per draw, so two samples in the same interval still differ.
    static DRAWS: AtomicU64 = AtomicU64::new(0);
    let seq = DRAWS.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9e37_79b9_7f4a_7c15);

    // The pool, folded, decides how long to wait. Drawing the wait length from
    // the pool is what keeps successive samples from being the same
    // measurement.
    let mut x = POOL.load(Ordering::Relaxed) ^ seq;
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    let target = x & 0x3ff;

    let hpet0 = crate::io::hpet::counter();
    let tsc0 = crate::io::tsc::rdtsc();
    // Busy-wait a bounded, variable number of times. `pause` is the
    // architectural hint for exactly this spin and lowers the memory-order cost
    // on an SMT sibling; the bound keeps a syscall from being able to hang the
    // machine.
    let mut i = 0u64;
    let mut tsc1 = tsc0;
    while i < target || (tsc1.wrapping_sub(tsc0) & 0x3f) != 0 {
        // SAFETY: `pause` is a hint with no architectural effect.
        unsafe { core::arch::asm!("pause", options(nomem, nostack)) };
        tsc1 = crate::io::tsc::rdtsc();
        i += 1;
        if i >= 4096 {
            break;
        }
    }
    let hpet1 = crate::io::hpet::counter();

    // Mix the two counter *deltas* and the trip count. Deltas rather than
    // absolute readings, because the absolute readings are shared state that
    // any two calls can see; the deltas are what this call actually observed.
    let dh = hpet1.wrapping_sub(hpet0);
    let dt = tsc1.wrapping_sub(tsc0);
    let mut acc = seq
        ^ dh.rotate_left((dh & 63) as u32)
        ^ dt.wrapping_mul(0x9e37_79b9_7f4a_7c15)
        ^ i.wrapping_mul(0xc2b2_ae3d_27d4_eb4f);
    acc ^= acc >> 31;
    acc = acc.wrapping_mul(0x94d0_49bb_1331_11eb);
    acc ^= acc >> 33;
    acc
}

/// SipHash-1-3 of `(seed, input)`, the mixing step for a hardware-free draw.
fn siphash13(seed: u64, input: u64) -> u64 {
    // The constants and rounds are SipHash-1-3 from the reference
    // implementation. This is not here to be a general-purpose hash: it is
    // here because its diffusion is strong enough that leaking part of the
    // output does not narrow the input, which a multiply-xorshift does not
    // give for free.
    const K0: u64 = 0x736f_6d65_7073_6575;
    const K1: u64 = 0x646f_7261_6e64_6f6d;
    const K2: u64 = 0x6c79_6765_6e65_7261;
    const K3: u64 = 0x7465_6462_7974_6573;

    let mut v0 = K0 ^ seed;
    let mut v1 = K1 ^ input;
    let mut v2 = K2 ^ input.rotate_left(32);
    let mut v3 = K3 ^ seed.rotate_left(32);

    macro_rules! round {
        ($v0:ident, $v1:ident, $v2:ident, $v3:ident) => {
            $v0 = $v0.wrapping_add($v1);
            $v1 = $v1.rotate_left(13);
            $v1 ^= $v0;
            $v0 = $v0.rotate_left(32);
            $v2 = $v2.wrapping_add($v3);
            $v3 = $v3.rotate_left(16);
            $v3 ^= $v2;
            $v0 = $v0.wrapping_add($v3);
            $v3 = $v3.rotate_left(21);
            $v3 ^= $v0;
            $v2 = $v2.wrapping_add($v1);
            $v1 = $v1.rotate_left(17);
            $v1 ^= $v2;
            $v2 = $v2.rotate_left(32);
        };
    }

    round!(v0, v1, v2, v3); // one compression round: this is SipHash-1-3

    // Finalization: the last input block, then the domain-separated final mix.
    v3 ^= input;
    round!(v0, v1, v2, v3);
    v0 ^= input;
    v2 ^= 0xff;
    round!(v0, v1, v2, v3);
    round!(v0, v1, v2, v3);
    round!(v0, v1, v2, v3);

    v0 ^ v1 ^ v2 ^ v3
}

/// Fill `out` with `out.len()` random bytes.
///
/// Returns `false` if no source is available, which the caller turns into
/// `ENOSYS`. A short fill is never reported as success: a caller that asked for
/// 32 bytes and got 8 has a buffer full of stale stack contents, which is the
/// same problem as never filling it at all.
pub fn fill(out: &mut [u8]) -> bool {
    if out.is_empty() {
        return true;
    }
    if source() == Source::None {
        return false;
    }
    // Seed the pool once from the first successful draw.
    if POOL_SEED.load(Ordering::Relaxed) == 0 {
        if let Some(v) = draw() {
            POOL_SEED.store(v | 1, Ordering::Release);
        }
    }
    for chunk in out.chunks_mut(8) {
        // Retry: an entropy instruction is allowed to report failure, and a
        // bounded number of attempts is the whole contract.
        let mut got = None;
        for _ in 0..64 {
            if let Some(v) = draw() {
                got = Some(v);
                break;
            }
        }
        let Some(v) = got else {
            return false;
        };
        // Mix through the pool so a caller never sees a raw counter value and
        // two requests in a row are not independent.
        let mixed = siphash13(POOL_SEED.load(Ordering::Relaxed), v);
        POOL.store(mixed, Ordering::Relaxed);
        let bytes = mixed.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    true
}

/// Whether a real entropy source is present, for a diagnostic.
pub fn have_source() -> bool {
    source() != Source::None
}

/// Name of the chosen source, for a diagnostic.
pub fn source_name() -> &'static str {
    match source() {
        Source::RdSeed => "rdseed",
        Source::RdRand => "rdrand",
        Source::Jitter => "hpet-jitter",
        Source::None => "none",
    }
}

/// One-shot boot diagnostic: which source was found, and does it actually work.
///
/// Probing availability is not the same as producing bytes -- an RDRAND
/// implementation can be present and starved -- so this draws once and says so.
pub fn diag() {
    let name = source_name();
    let mut probe = [0u8; 8];
    let works = fill(&mut probe);
    if works {
        crate::log::kdebug!("entropy: source={} (drew {} bytes)", name, probe.len());
    } else {
        crate::log::kwarn!(
            "entropy: source={} but a draw failed; getrandom will report ENOSYS",
            name
        );
    }
}
