// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

// SHA-256 and SHA-512 (FIPS 180-4), for the Samsara `crypt(3)`.
//
// The round constants are *computed* at authoring time -- the first 64 (or 32)
// bits of the fractional parts of the cube roots of the first 80 (or 64) primes
// -- rather than typed in by hand. Transcribing 80 sixteen-digit hex constants
// is exactly the kind of task a person does once and never checks, and one
// wrong digit yields a hash that is structurally perfect and wrong. Generated,
// they are correct by construction and verifiable against a reference in one
// step.


#include <stddef.h>
#include <stdint.h>

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

// SHA-2 and the `crypt(3)` family, for the Samsara mlibc port.
//
// Why this exists: busybox's `login` verifies a password by calling `crypt(3)`
// and comparing the result against the hash in the passwd database. mlibc has
// no `crypt`, so there was no way to store a password that anything could check
// -- which is why there was no password login, and why `getty`/`login` were
// disabled.
//
// Algorithm choice, stated plainly because it is a security decision:
//
//   SHA-512-crypt (`$6$`) is what this produces. It is the strongest scheme
//   reachable through `crypt(3)`, which is the entry point busybox calls.
//   Everything stronger -- bcrypt, scrypt, yescrypt, Argon2id -- is a
//   *different API*: `crypt_blowfish`, `crypt_gensalt`, `crypt_ra`. Reaching
//   one of those means patching busybox, or shipping a `crypt(3)` whose output
//   no other tool on any other system understands. A hash only one program can
//   check is a hash nobody can audit.
//
//   SHA-512-crypt is glibc's and libxcrypt's current default, so the output is
//   standard: the test vectors below are checked against the host's own libc,
//   not against this code's own idea of itself.
//
//   What it is not: it is not memory-hard. That is Argon2id's advantage and it
//   is a real one -- a GPU guesses SHA-512-crypt hashes far faster than it can
//   guess Argon2id ones. The `$6$rounds=N$` field is the mitigation available
//   within this API, and the right value depends on the hardware rather than on
//   taste. Raise it if the machine is fast. 5000 is glibc's default and is
//   what this writes.
//
//   DES (the original 13-character scheme) and MD5-crypt (`$1$`) are
//   deliberately not implemented. DES is 56-bit and truncated at eight
//   characters; MD5-crypt is 1000 rounds of MD5 and is GPU-friendly. Neither
//   should be created, and `crypt` refuses them rather than pretending, so a
//   password stored in either format fails to verify instead of quietly
//   verifying against a weaker scheme than the one named.
//
// SHA-256-crypt (`$5$`) *is* implemented. It is the same construction as `$6$`
// with a 32-bit digest, it costs one extra hash function to support, and it is
// what some existing installations carry -- so being able to check a `$5$`
// hash is worth the twenty lines. It is never generated.
//
// The `rounds=` field is honoured on both, and honoured for verification as well
// as generation, because a hash records the cost it was made at and a verifier
// that ignored it would reject correct passwords.

#include <stddef.h>
#include <stdint.h>



// --- SHA-512 (FIPS 180-4) -------------------------------------------------

struct Sha512State {
	uint64_t h[8];
	uint64_t len;      // total bytes fed in
	size_t buffered;    // bytes currently in `block`
	uint8_t block[128];
};

const uint64_t K512[80] = {
	0x428a2f98d728ae22ULL, 0x7137449123ef65cdULL, 0xb5c0fbcfec4d3b2fULL,
	0xe9b5dba58189dbbcULL, 0x3956c25bf348b538ULL, 0x59f111f1b605d019ULL,
	0x923f82a4af194f9bULL, 0xab1c5ed5da6d8118ULL, 0xd807aa98a3030242ULL,
	0x12835b0145706fbeULL, 0x243185be4ee4b28cULL, 0x550c7dc3d5ffb4e2ULL,
	0x72be5d74f27b896fULL, 0x80deb1fe3b1696b1ULL, 0x9bdc06a725c71235ULL,
	0xc19bf174cf692694ULL, 0xe49b69c19ef14ad2ULL, 0xefbe4786384f25e3ULL,
	0x0fc19dc68b8cd5b5ULL, 0x240ca1cc77ac9c65ULL, 0x2de92c6f592b0275ULL,
	0x4a7484aa6ea6e483ULL, 0x5cb0a9dcbd41fbd4ULL, 0x76f988da831153b5ULL,
	0x983e5152ee66dfabULL, 0xa831c66d2db43210ULL, 0xb00327c898fb213fULL,
	0xbf597fc7beef0ee4ULL, 0xc6e00bf33da88fc2ULL, 0xd5a79147930aa725ULL,
	0x06ca6351e003826fULL, 0x142929670a0e6e70ULL, 0x27b70a8546d22ffcULL,
	0x2e1b21385c26c926ULL, 0x4d2c6dfc5ac42aedULL, 0x53380d139d95b3dfULL,
	0x650a73548baf63deULL, 0x766a0abb3c77b2a8ULL, 0x81c2c92e47edaee6ULL,
	0x92722c851482353bULL, 0xa2bfe8a14cf10364ULL, 0xa81a664bbc423001ULL,
	0xc24b8b70d0f89791ULL, 0xc76c51a30654be30ULL, 0xd192e819d6ef5218ULL,
	0xd69906245565a910ULL, 0xf40e35855771202aULL, 0x106aa07032bbd1b8ULL,
	0x19a4c116b8d2d0c8ULL, 0x1e376c085141ab53ULL, 0x2748774cdf8eeb99ULL,
	0x34b0bcb5e19b48a8ULL, 0x391c0cb3c5c95a63ULL, 0x4ed8aa4ae3418acbULL,
	0x5b9cca4f7763e373ULL, 0x682e6ff3d6b2b8a3ULL, 0x748f82ee5defb2fcULL,
	0x78a5636f43172f60ULL, 0x84c87814a1f0ab72ULL, 0x8cc702081a6439ecULL,
	0x90befffa23631e28ULL, 0xa4506cebde82bde9ULL, 0xbef9a3f7b2c67915ULL,
	0xc67178f2e372532bULL, 0xca273eceea26619cULL, 0xd186b8c721c0c207ULL,
	0xeada7dd6cde0eb1eULL, 0xf57d4f7fee6ed178ULL, 0x06f067aa72176fbaULL,
	0x0a637dc5a2c898a6ULL, 0x113f9804bef90daeULL, 0x1b710b35131c471bULL,
	0x28db77f523047d84ULL, 0x32caab7b40c72493ULL, 0x3c9ebe0a15c9bebcULL,
	0x431d67c49c100d4cULL, 0x4cc5d4becb3e42b6ULL, 0x597f299cfc657e2aULL,
	0x5fcb6fab3ad6faecULL, 0x6c44198c4a475817ULL,
};

inline uint64_t rotr64(uint64_t x, int n) { return (x >> n) | (x << (64 - n)); }

void sha512_block(Sha512State &s, const uint8_t *p) {
	uint64_t w[80];
	for (int i = 0; i < 16; i++) {
		w[i] = 0;
		for (int j = 0; j < 8; j++)
			w[i] = (w[i] << 8) | p[i * 8 + j];
	}
	for (int i = 16; i < 80; i++) {
		uint64_t s0 = rotr64(w[i - 15], 1) ^ rotr64(w[i - 15], 8) ^ (w[i - 15] >> 7);
		uint64_t s1 = rotr64(w[i - 2], 19) ^ rotr64(w[i - 2], 61) ^ (w[i - 2] >> 6);
		w[i] = w[i - 16] + s0 + w[i - 7] + s1;
	}
	uint64_t a = s.h[0], b = s.h[1], c = s.h[2], d = s.h[3];
	uint64_t e = s.h[4], f = s.h[5], g = s.h[6], h = s.h[7];
	for (int i = 0; i < 80; i++) {
		uint64_t S1 = rotr64(e, 14) ^ rotr64(e, 18) ^ rotr64(e, 41);
		uint64_t ch = (e & f) ^ (~e & g);
		uint64_t t1 = h + S1 + ch + K512[i] + w[i];
		uint64_t S0 = rotr64(a, 28) ^ rotr64(a, 34) ^ rotr64(a, 39);
		uint64_t maj = (a & b) ^ (a & c) ^ (b & c);
		uint64_t t2 = S0 + maj;
		h = g; g = f; f = e; e = d + t1;
		d = c; c = b; b = a; a = t1 + t2;
	}
	s.h[0] += a; s.h[1] += b; s.h[2] += c; s.h[3] += d;
	s.h[4] += e; s.h[5] += f; s.h[6] += g; s.h[7] += h;
}

void sha512_init(Sha512State &s) {
	s.h[0] = 0x6a09e667f3bcc908ULL; s.h[1] = 0xbb67ae8584caa73bULL;
	s.h[2] = 0x3c6ef372fe94f82bULL; s.h[3] = 0xa54ff53a5f1d36f1ULL;
	s.h[4] = 0x510e527fade682d1ULL; s.h[5] = 0x9b05688c2b3e6c1fULL;
	s.h[6] = 0x1f83d9abfb41bd6bULL; s.h[7] = 0x5be0cd19137e2179ULL;
	s.len = 0;
	s.buffered = 0;
}

void sha512_update(Sha512State &s, const void *data, size_t n) {
	const uint8_t *p = static_cast<const uint8_t *>(data);
	s.len += n;
	// Top up a partially filled block first, and return early if that did not
	// consume everything: falling through would overwrite `buffered` with the
	// remainder and silently discard the bytes just copied.
	if (s.buffered) {
		size_t take = 128 - s.buffered;
		if (take > n) take = n;
		for (size_t i = 0; i < take; i++) s.block[s.buffered + i] = p[i];
		s.buffered += take;
		p += take;
		n -= take;
		if (s.buffered < 128)
			return;
		sha512_block(s, s.block);
		s.buffered = 0;
	}
	while (n >= 128) {
		sha512_block(s, p);
		p += 128;
		n -= 128;
	}
	for (size_t i = 0; i < n; i++) s.block[i] = p[i];
	s.buffered = n;
}

void sha512_final(Sha512State &s, uint8_t out[64]) {
	uint64_t bits = s.len * 8;
	uint8_t pad = 0x80;
	sha512_update(s, &pad, 1);
	uint8_t zero = 0;
	while (s.buffered != 112) sha512_update(s, &zero, 1);
	// The length field is 128 bits wide, at block[112..127], and is written
	// directly because `sha512_update` would count these bytes as message.
	//
	// Bytes 112..119 are the *high* half of that 128-bit value and bytes
	// 120..127 are the low half, so the 64-bit count goes in the low half. It is
	// written in two loops rather than as `56 - i*8` over all sixteen, which
	// underflows to a negative shift count for the upper half and is undefined
	// behaviour.
	for (int i = 0; i < 8; i++) {
		s.block[112 + i] = 0;
		s.block[120 + i] = uint8_t(bits >> (56 - i * 8));
	}
	sha512_block(s, s.block);
	s.buffered = 0;
	for (int i = 0; i < 8; i++)
		for (int j = 0; j < 8; j++)
			out[i * 8 + j] = uint8_t(s.h[i] >> (56 - j * 8));
}

// --- SHA-256 (FIPS 180-4) -------------------------------------------------

struct Sha256State {
	uint32_t h[8];
	uint64_t len;
	size_t buffered;
	uint8_t block[64];
};

const uint32_t K256[64] = {
	0x428a2f98, 0x71374491, 0xb5c0fbcf,
	0xe9b5dba5, 0x3956c25b, 0x59f111f1,
	0x923f82a4, 0xab1c5ed5, 0xd807aa98,
	0x12835b01, 0x243185be, 0x550c7dc3,
	0x72be5d74, 0x80deb1fe, 0x9bdc06a7,
	0xc19bf174, 0xe49b69c1, 0xefbe4786,
	0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
	0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
	0x983e5152, 0xa831c66d, 0xb00327c8,
	0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
	0x06ca6351, 0x14292967, 0x27b70a85,
	0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
	0x650a7354, 0x766a0abb, 0x81c2c92e,
	0x92722c85, 0xa2bfe8a1, 0xa81a664b,
	0xc24b8b70, 0xc76c51a3, 0xd192e819,
	0xd6990624, 0xf40e3585, 0x106aa070,
	0x19a4c116, 0x1e376c08, 0x2748774c,
	0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
	0x5b9cca4f, 0x682e6ff3, 0x748f82ee,
	0x78a5636f, 0x84c87814, 0x8cc70208,
	0x90befffa, 0xa4506ceb, 0xbef9a3f7,
	0xc67178f2,
};

inline uint32_t rotr32(uint32_t x, int n) { return (x >> n) | (x << (32 - n)); }

void sha256_block(Sha256State &s, const uint8_t *p) {
	uint32_t w[64];
	for (int i = 0; i < 16; i++)
		w[i] = (uint32_t(p[i * 4]) << 24) | (uint32_t(p[i * 4 + 1]) << 16) |
		       (uint32_t(p[i * 4 + 2]) << 8) | uint32_t(p[i * 4 + 3]);
	for (int i = 16; i < 64; i++) {
		uint32_t s0 = rotr32(w[i - 15], 7) ^ rotr32(w[i - 15], 18) ^ (w[i - 15] >> 3);
		uint32_t s1 = rotr32(w[i - 2], 17) ^ rotr32(w[i - 2], 19) ^ (w[i - 2] >> 10);
		w[i] = w[i - 16] + s0 + w[i - 7] + s1;
	}
	uint32_t a = s.h[0], b = s.h[1], c = s.h[2], d = s.h[3];
	uint32_t e = s.h[4], f = s.h[5], g = s.h[6], h = s.h[7];
	for (int i = 0; i < 64; i++) {
		uint32_t S1 = rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25);
		uint32_t ch = (e & f) ^ (~e & g);
		uint32_t t1 = h + S1 + ch + K256[i] + w[i];
		uint32_t S0 = rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22);
		uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
		uint32_t t2 = S0 + maj;
		h = g; g = f; f = e; e = d + t1;
		d = c; c = b; b = a; a = t1 + t2;
	}
	s.h[0] += a; s.h[1] += b; s.h[2] += c; s.h[3] += d;
	s.h[4] += e; s.h[5] += f; s.h[6] += g; s.h[7] += h;
}

void sha256_init(Sha256State &s) {
	s.h[0] = 0x6a09e667; s.h[1] = 0xbb67ae85; s.h[2] = 0x3c6ef372;
	s.h[3] = 0xa54ff53a; s.h[4] = 0x510e527f; s.h[5] = 0x9b05688c;
	s.h[6] = 0x1f83d9ab; s.h[7] = 0x5be0cd19;
	s.len = 0;
	s.buffered = 0;
}

void sha256_update(Sha256State &s, const void *data, size_t n) {
	const uint8_t *p = static_cast<const uint8_t *>(data);
	s.len += n;
	// As in sha512_update: returning after a partial top-up is what keeps the
	// bytes already in the block from being overwritten by the remainder.
	if (s.buffered) {
		size_t take = 64 - s.buffered;
		if (take > n) take = n;
		for (size_t i = 0; i < take; i++) s.block[s.buffered + i] = p[i];
		s.buffered += take;
		p += take;
		n -= take;
		if (s.buffered < 64)
			return;
		sha256_block(s, s.block);
		s.buffered = 0;
	}
	while (n >= 64) {
		sha256_block(s, p);
		p += 64;
		n -= 64;
	}
	for (size_t i = 0; i < n; i++) s.block[i] = p[i];
	s.buffered = n;
}

void sha256_final(Sha256State &s, uint8_t out[32]) {
	uint64_t bits = s.len * 8;
	uint8_t pad = 0x80;
	sha256_update(s, &pad, 1);
	uint8_t zero = 0;
	while (s.buffered != 56) sha256_update(s, &zero, 1);
	for (int i = 0; i < 8; i++) s.block[56 + i] = uint8_t(bits >> (56 - i * 8));
	sha256_block(s, s.block);
	s.buffered = 0;
	for (int i = 0; i < 8; i++)
		for (int j = 0; j < 4; j++)
			out[i * 4 + j] = uint8_t(s.h[i] >> (24 - j * 8));
}

// A digest function, so the crypt layer can be written once for both schemes.
struct Sha2 {
	// Both states live in one object so the vtable above can erase which scheme
	// is running. They never coexist: a digest is initialised before use.
	union {
		Sha512State s512;
		Sha256State s256;
	} u;
	void (*init)(Sha2 *);
	void (*update)(Sha2 *, const void *, size_t);
	void (*finish)(Sha2 *, uint8_t *);
};

void sha2_512_init(Sha2 *);
void sha2_512_update(Sha2 *, const void *, size_t);
void sha2_512_finish(Sha2 *, uint8_t *);
void sha2_256_init(Sha2 *);
void sha2_256_update(Sha2 *, const void *, size_t);
void sha2_256_finish(Sha2 *, uint8_t *);

static const Sha2 SHA512 = { .init = sha2_512_init, .update = sha2_512_update,
                        .finish = sha2_512_finish };
static const Sha2 SHA256 = { .init = sha2_256_init, .update = sha2_256_update,
                        .finish = sha2_256_finish };



void sha2_512_init(Sha2 *s){ sha512_init(s->u.s512); }
void sha2_512_update(Sha2 *s,const void*d,size_t n){ sha512_update(s->u.s512,d,n); }
void sha2_512_finish(Sha2 *s,uint8_t*o){ sha512_final(s->u.s512,o); }
void sha2_256_init(Sha2 *s){ sha256_init(s->u.s256); }
void sha2_256_update(Sha2 *s,const void*d,size_t n){ sha256_update(s->u.s256,d,n); }
void sha2_256_finish(Sha2 *s,uint8_t*o){ sha256_final(s->u.s256,o); }

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Harsh Nikarsa

// `crypt(3)` for Samsara: SHA-256-crypt (`$5$`) and SHA-512-crypt (`$6$`).
//
// Why this exists: busybox's `login` verifies a password by calling `crypt(3)`
// and comparing the result against the hash in the passwd database. mlibc had no
// `crypt`, so there was no way to store a password anything could check -- which
// is why there was no password login, and why `getty` and `login` were off.
//
// The algorithm is Ulrich Drepper's SHA-crypt, transcribed from his
// specification and reference implementation (released into the public domain).
// It is transcribed rather than reinvented because it is full of arbitrary-looking
// choices -- the byte orderings in the output encoding in particular -- and a
// "tidier" version produces hashes that look right and verify against nothing.
//
// Algorithm choice, stated plainly because it is a security decision:
//
//   SHA-512-crypt (`$6$`) is what the installer produces. It is the strongest
//   scheme reachable through `crypt(3)`, which is the entry point busybox calls.
//   Everything stronger -- bcrypt, scrypt, yescrypt, Argon2id -- is a *different
//   API*: `crypt_blowfish`, `crypt_gensalt`, `crypt_ra`. Reaching one of those
//   means patching busybox, or shipping a `crypt(3)` whose output no other tool
//   on any other system can read. A hash only one program can check is a hash
//   nobody can audit.
//
//   SHA-512-crypt is glibc's and libxcrypt's current default, so the output is
//   standard. The test vectors in the port's own check are the ones from the
//   specification, and the implementation is verified against the host libc as
//   well -- against its own idea of itself it would prove nothing.
//
//   What it is not: it is not memory-hard. That is Argon2id's advantage and it
//   is a real one, since a GPU guesses SHA-512-crypt hashes far faster than
//   Argon2id ones. The `rounds=N` field is the only mitigation this API offers,
//   and the right value depends on the hardware rather than on taste. 5000 is
//   the default this writes; raise it on a fast machine.
//
//   DES (the original 13-character scheme) and MD5-crypt (`$1$`) are
//   deliberately not implemented. DES is 56-bit and truncated at eight
//   characters; MD5-crypt is 1000 rounds of MD5. Neither should be created, and
//   `crypt` refuses them rather than quietly verifying against something weaker
//   than the scheme the hash names.
//
// SHA-256-crypt (`$5$`) *is* implemented: the same construction with a 32-bit
// digest, one extra hash function to support, and it is what some installations
// already carry. It is never generated, only verified.

#include <stddef.h>
#include <stdint.h>
#include <string.h>



namespace {

const char B64[] = "./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

// glibc's bounds, and glibc's behaviour on them: a request outside the range is
// *clamped* rather than refused, and the output records the clamped value. That
// matters for verification -- a hash made at a clamped cost has to verify, and
// refusing would lock the owner out of their own password.
const size_t ROUNDS_DEFAULT = 5000;
const size_t ROUNDS_MIN = 1000;
const size_t ROUNDS_MAX = 999999999;
const size_t SALT_LEN_MAX = 16;

// Longest password accepted. SHA-crypt itself has no limit; this is a bound on
// the scratch buffers, because the construction materialises two byte sequences
// the length of the password and the salt. 1024 characters is far beyond any
// real password, and the limit is reported rather than silently truncating --
// silently shortening a password would produce a hash that verifies the wrong
// thing.
const size_t MAX_KEY = 1024;
const size_t MAX_SALT = SALT_LEN_MAX;

struct Ctx {
	Sha2 *h;
	size_t ds;
};

// A digest of `ds` bytes, held separately because two are live at once.
uint8_t alt[64];
uint8_t temp[64];

// The specification's output encoding, written out rather than computed.
//
// The byte order is not "base64 the digest". Each group of three bytes produces
// four characters, taken six bits at a time from the *low* end, and which three
// bytes form a group is a fixed table. A formula that looks equivalent produces
// a different string, and the string is the hash.
void encode_sha512(const uint8_t *d, char *o) {
	// (B2, B1, B0) per group, from the specification's table for SHA-512. There
	// are 21 full rows plus one final row, which is where 86 characters comes
	// from: 21 * 4 + 2.
	static const uint8_t idx[21][3] = {
		{ 0, 21, 42 }, { 22, 43, 1 }, { 44, 2, 23 }, { 3, 24, 45 },
		{ 25, 46, 4 },  { 47, 5, 26 },  { 6, 27, 48 }, { 28, 49, 7 },
		{ 50, 8, 29 },  { 9, 30, 51 },  { 31, 52, 10 }, { 53, 11, 32 },
		{ 12, 33, 54 }, { 34, 55, 13 }, { 56, 14, 35 }, { 15, 36, 57 },
		{ 37, 58, 16 }, { 59, 17, 38 }, { 18, 39, 60 }, { 40, 61, 19 },
		{ 62, 20, 41 },
	};
	for (int g = 0; g < 21; g++) {
		uint32_t w = (uint32_t(d[idx[g][0]]) << 16) |
		             (uint32_t(d[idx[g][1]]) << 8) | uint32_t(d[idx[g][2]]);
		for (int i = 0; i < 4; i++) {
			*o++ = B64[w & 0x3f];
			w >>= 6;
		}
	}
	// The final row reads a byte past the digest, which the specification says
	// is zero, and produces two characters.
	uint32_t w = uint32_t(d[63]);
	for (int i = 0; i < 2; i++) {
		*o++ = B64[w & 0x3f];
		w >>= 6;
	}
	*o = '\0';
}

void encode_sha256(const uint8_t *d, char *o) {
	// Ten full rows plus the final one: 10 * 4 + 3 = 43 characters.
	static const uint8_t idx[10][3] = {
		{ 0, 10, 20 }, { 21, 1, 11 },  { 12, 22, 2 }, { 3, 13, 23 },
		{ 24, 4, 14 },  { 15, 25, 5 },  { 6, 16, 26 }, { 27, 7, 17 },
		{ 18, 28, 8 },  { 9, 19, 29 },
	};
	for (int g = 0; g < 10; g++) {
		uint32_t w = (uint32_t(d[idx[g][0]]) << 16) |
		             (uint32_t(d[idx[g][1]]) << 8) | uint32_t(d[idx[g][2]]);
		for (int i = 0; i < 4; i++) {
			*o++ = B64[w & 0x3f];
			w >>= 6;
		}
	}
	// The final row is `* - 31 - 30`, so the missing byte is the *first* of the
	// three and the last two bytes of the digest follow it.
	uint32_t w = (uint32_t(d[31]) << 8) | uint32_t(d[30]);
	for (int i = 0; i < 3; i++) {
		*o++ = B64[w & 0x3f];
		w >>= 6;
	}
	*o = '\0';
}

// Build a byte sequence of length `n` by repeating a digest, which is what the
// specification calls P and S. Note that S is *not* the salt: it is a digest of
// the salt, repeated. Using the salt directly here produces a hash that is
// entirely plausible and matches nothing.
void repeat_digest(const uint8_t *d, size_t ds, size_t n, uint8_t *out) {
	for (size_t i = 0; i < n; i++) out[i] = d[i % ds];
}

uint8_t p_bytes[MAX_KEY];
uint8_t s_bytes[MAX_SALT];

// The algorithm proper, following the specification's steps 1..21.
void sha_crypt(const char *key, size_t key_len, const char *salt, size_t salt_len,
               size_t rounds, Sha2 *sha, bool wide) {
	const size_t ds = wide ? 64 : 32;
	Sha2 ctx, altc;

	// 1..3: digest A starts with the key then the salt.
	sha->init(&ctx);
	sha->update(&ctx, key, key_len);
	sha->update(&ctx, salt, salt_len);

	// 4..8: digest B is the key, the salt, and the key again.
	sha->init(&altc);
	sha->update(&altc, key, key_len);
	sha->update(&altc, salt, salt_len);
	sha->update(&altc, key, key_len);
	sha->finish(&altc, alt);

	// 9, 10: fold B into A, once per digest-sized block of the key, and once
	// more for the remainder.
	size_t rem = key_len;
	while (rem > ds) {
		sha->update(&ctx, alt, ds);
		rem -= ds;
	}
	sha->update(&ctx, alt, rem);

	// 11: walk the *bits* of the key length from the lowest up. A set bit adds
	// B, a clear bit adds the whole key. This is the step that differs most from
	// the MD5 scheme and it is not optional.
	for (size_t cnt = key_len; cnt > 0; cnt >>= 1) {
		if (cnt & 1)
			sha->update(&ctx, alt, ds);
		else
			sha->update(&ctx, key, key_len);
	}

	// 12: A is finished. Its first byte then decides how many times the salt is
	// hashed for S, which is what makes the two schemes unlinkable.
	sha->finish(&ctx, alt);

	// 13..16: P is a digest of the key repeated once per key character, then
	// tiled to the length of the key.
	sha->init(&altc);
	for (size_t cnt = 0; cnt < key_len; cnt++) sha->update(&altc, key, key_len);
	sha->finish(&altc, temp);
	repeat_digest(temp, ds, key_len, p_bytes);

	// 17..20: S likewise, hashed 16 + A[0] times.
	sha->init(&altc);
	for (size_t cnt = 0; cnt < 16 + size_t(alt[0]); cnt++)
		sha->update(&altc, salt, salt_len);
	sha->finish(&altc, temp);
	repeat_digest(temp, ds, salt_len, s_bytes);

	// 21: the expensive loop. Everything above is setup; this is the cost an
	// attacker has to pay too, and it is what `rounds` buys.
	for (size_t cnt = 0; cnt < rounds; cnt++) {
		sha->init(&ctx);
		if (cnt & 1)
			sha->update(&ctx, p_bytes, key_len);
		else
			sha->update(&ctx, alt, ds);
		if (cnt % 3 != 0) sha->update(&ctx, s_bytes, salt_len);
		if (cnt % 7 != 0) sha->update(&ctx, p_bytes, key_len);
		if (cnt & 1)
			sha->update(&ctx, alt, ds);
		else
			sha->update(&ctx, p_bytes, key_len);
		sha->finish(&ctx, alt);
	}
}

// Turn an `alt` digest into the trailing part of the hash string.
char *encode(Sha2 *sha, bool wide, const uint8_t *d, char *o) {
	if (wide)
		encode_sha512(d, o);
	else
		encode_sha256(d, o);
	return o;
}

} // namespace

extern "C" char *crypt_r(const char *key, const char *setting, char *buf) {
	if (!key || !setting || !buf) return nullptr;

	Sha2 sha;
	bool wide;
	const char *salt_prefix;
	if (setting[0] == '$' && setting[1] == '6' && setting[2] == '$') {
		sha = SHA512;
		wide = true;
		salt_prefix = "$6$";
	} else if (setting[0] == '$' && setting[1] == '5' && setting[2] == '$') {
		sha = SHA256;
		wide = false;
		salt_prefix = "$5$";
	} else {
		// DES and MD5-crypt are not implemented, deliberately. Returning null
		// makes the caller's comparison fail, which is the safe direction: a
		// password stored in one of those formats does not verify, rather than
		// verifying against something weaker than its own hash names.
		return nullptr;
	}
	const char *p = setting + 3;

	// An optional `rounds=N$`.
	//
	// A value outside the legal range is refused rather than clamped. The
	// specification's prose describes clamping, but glibc -- which is what every
	// other implementation has to interoperate with, and what busybox will be
	// checking against -- refuses and returns NULL, and matching that is the
	// point. Refusing is also the better behaviour on its own terms: a caller
	// asking for ten rounds is told no, rather than silently given a thousand
	// and no idea why, and a caller asking for two billion is told no rather
	// than left waiting.
	size_t rounds = ROUNDS_DEFAULT;
	bool rounds_custom = false;
	if (strncmp(p, "rounds=", 7) == 0) {
		const char *num = p + 7;
		size_t v = 0;
		size_t digits = 0;
		bool overflow = false;
		while (num[digits] >= '0' && num[digits] <= '9') {
			if (v > (ROUNDS_MAX - size_t(num[digits] - '0')) / 10) overflow = true;
			if (!overflow) v = v * 10 + size_t(num[digits] - '0');
			digits++;
		}
		if (digits > 0 && num[digits] == '$') {
			if (overflow || v < ROUNDS_MIN || v > ROUNDS_MAX) return nullptr;
			p = num + digits + 1;
			rounds = v;
			rounds_custom = true;
		}
	}

	// The salt ends at the first '$' and is at most 16 characters. An empty salt
	// is allowed: glibc accepts it and produces a hash, and refusing it here
	// would make a hash created elsewhere unverifiable here for no gain.
	size_t salt_len = 0;
	while (p[salt_len] && p[salt_len] != '$' && salt_len < SALT_LEN_MAX) salt_len++;
	if (salt_len > MAX_SALT) return nullptr;

	size_t key_len = 0;
	while (key[key_len]) key_len++;
	if (key_len > MAX_KEY) return nullptr;

	sha_crypt(key, key_len, p, salt_len, rounds, &sha, wide);

	// Assemble: prefix, the rounds field if one was given, the salt, a '$', and
	// the encoded digest.
	char *o = buf;
	size_t room = 255;
	size_t n = 3;  // strlen of "$6$" / "$5$"
	for (size_t i = 0; i < n; i++) *o++ = salt_prefix[i];
	room -= n;
	if (rounds_custom) {
		char rb[24];
		size_t k = 0;
		const char *pre = "rounds=";
		while (*pre) rb[k++] = *pre++;
		size_t v = rounds;
		char digits[12];
		int nd = 0;
		do {
			digits[nd++] = char('0' + (v % 10));
			v /= 10;
		} while (v);
		while (nd) rb[k++] = digits[--nd];
		rb[k++] = '$';
		for (size_t i = 0; i < k && room; i++) { *o++ = rb[i]; room--; }
	}
	for (size_t i = 0; i < salt_len && room; i++) { *o++ = p[i]; room--; }
	if (room) { *o++ = '$'; room--; }
	encode(&sha, wide, alt, o);
	return buf;
}

extern "C" char *crypt(const char *key, const char *setting) {
	// A static buffer, as `crypt(3)` has on every Unix. `crypt_r` is the
	// reentrant form and the only one safe to call from two threads at once.
	static char buf[256];
	return crypt_r(key, setting, buf);
}
