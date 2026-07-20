# Post-Quantum Cryptography Design: ML-KEM (FIPS 203) & ML-DSA (FIPS 204) for Sond

> **Purpose:** This document is the PLAN only. It maps the ML-KEM and ML-DSA standard
> algorithms to Sond modules, function signatures, dependencies, test vectors, and
> implementation challenges, so that a distributed fleet can implement the crates in
> parallel and integrate without merge conflicts.

---

## 1 Parameter Sets Overview

Both algorithms operate over the polynomial ring Rq = Zq[x] / (x^n + 1) with n = 256
fixed. They differ in modulus q, module rank, and security level.

### 1.1 ML-KEM (FIPS 203)

| Param Set  | NIST Level | Security ~  | k (rank) | Encapped Key | Decapped Key | Ciphertext | Shared Secret |
|------------|------------|-------------|----------|--------------|--------------|------------|---------------|
| ML-KEM-512 | 1          | AES-128     | 2        | 800 B        | 1632 B       | 768 B      | 32 B          |
| ML-KEM-768 | 3          | AES-192     | 3        | 1184 B       | 2400 B       | 1088 B     | 32 B          |
| ML-KEM-1024| 5          | AES-256     | 4        | 1568 B       | 3168 B       | 1568 B     | 32 B          |

Ring parameters (all three): n = 256, q = 3329, eta = 2.

### 1.2 ML-DSA (FIPS 204)

| Param Set | NIST Level | Security ~        | l | L | Public Key | Private Key | Signature |
|-----------|------------|-------------------|---|---|------------|-------------|-----------|
| ML-DSA-44 | 1          | AES-128 / SHA-256 | 4 | 4 | 1312 B     | 2560 B      | 2420 B    |
| ML-DSA-65 | 3          | AES-192 / SHA-384 | 6 | 5 | 1952 B     | 4032 B      | 3309 B    |
| ML-DSA-87 | 5          | AES-256 / SHA-512 | 8 | 7 | 2592 B     | 4896 B      | 4627 B    |

Ring parameters (all three): n = 256, q = 8380417, zeta = 17 (primitive 256th root of
unity mod q: 8380417 = 6 * 2^23 + 17).

Per-set constants:

| Param         | ML-DSA-44 | ML-DSA-65 | ML-DSA-87 |
|---------------|-----------|-----------|-----------|
| l             | 4         | 6         | 8         |
| L             | 4         | 5         | 7         |
| eta           | 2         | 2         | 1         |
| gamma1        | 2^19      | 2^17      | 2^10      |
| gamma2        | 2^15      | 2^15      | 2^10      |
| kappa         | 7         | 10        | 6         |
| tau           | 60        | 50        | 60        |
| omega         | 75        | 60        | 50        |
| hash function | SHA-256   | SHA-384   | SHA-512   |

---

## 2 Module Map

```
lib/any/crypto/hash/
    shake.snd              <- NEW: SHAKE128 / SHAKE256 XOF

lib/any/crypto/mlkem/
    mlkem-poly.snd         <- NEW: field arithmetic mod 3329, NTT, CBD
    mlkem-encode.snd       <- NEW: 12-bit pack/unpack, matrix A encoding
    mlkem-pke.snd          <- NEW: K-PKE (public-key encryption core)
    mlkem-kem.snd          <- NEW: ML-KEM KeyGen / Encaps / Decaps
    mlkem_test.snd         <- NEW: KAT test vectors

lib/any/crypto/mldsa/
    mldsa-poly.snd         <- NEW: field arithmetic mod 8380417, NTT, zetas
    mldsa-sampling.snd     <- NEW: rejection sampling, ExpandS, ExpandMask, PRF
    mldsa-pke.snd          <- NEW: PQPKE (public-key encryption for ML-DSA)
    mldsa-sig.snd          <- NEW: ML-DSA KeyGen / Sign / Verify
    mldsa_test.snd         <- NEW: KAT test vectors
```

### 2.1 Module Dependency Graph

shake.snd -> mlkem-poly.snd -> mlkem-encode.snd -> mlkem-pke.snd -> mlkem-kem.snd
shake.snd -> mldsa-poly.snd -> mldsa-sampling.snd -> mldsa-pke.snd -> mldsa-sig.snd

No cross-module coupling between ML-KEM and ML-DSA (besides shared shake.snd).

---

## 3 FIPS 203: ML-KEM

### 3.1 mlkem-poly.snd: Core Polynomial Module (mod q=3329)

Polynomials: 256 i64 words. Each a[i] in [0, 3329) after reduction.

Module-level constants:

```sond
let MLKEM_Q = 3329
let MLKEM_N = 256
let MLKEM_ZETAS[128]   // NTT twiddles, Montgomery form, FIPS 203 Appendix A, Table 2
```

| Function | Description | Reference |
|----------|-------------|-----------|
| mlkem_reduce(a) | Barrett: a mod 3329 (input any i64, output [0, 3329)) | FIPS 7.1.1 |
| mlkem_mont_mul(a, b) | a * b * R^-1 mod q, R=2^15 | FIPS 7.1.1 |
| mlkem_mont_sq(a) | a * a * R^-1 mod q | FIPS 7.1.1 |
| mlkem_montgomery_reduce(t) | Reduce 2-word value to Montgomery form | FIPS 7.1.1 |
| mlkem_poly_add(r, a, b) | r[i] = (a[i]+b[i]) mod q | FIPS 7.1.1 |
| mlkem_poly_sub(r, a, b) | r[i] = (a[i]-b[i]+q) mod q | FIPS 7.1.1 |
| mlkem_poly_ntt(r, a) | Forward NTT: ring -> NTT (Montgomery) | FIPS 7.1.2, Alg 4 |
| mlkem_poly_intt(r, a) | Inverse NTT: NTT -> ring (Montgomery) | FIPS 7.1.3, Alg 5 |
| mlkem_poly_pointwise(r, a, b) | r[i] = NTT(a)[i] * NTT(b)[i] * R^-1 mod q | FIPS 7.1.1 |
| mlkem_poly_cbd(r, z) | CBD eta=2: sample k=2..4 polys from z bytes | FIPS A.3.2, Alg 6 |
| mlkem_poly_compress(r, a, d) | Compress coeff to d bits | FIPS 7.1.4, Alg 8 |
| mlkem_poly_decompress(r, a, d) | Decompress d-bit coeffs | FIPS 7.1.4, Alg 9 |

#### 3.1.2 NTT (FIPS A.3.1)

128 precomputed zetas in Montgomery form (each = zeta_R * R mod 3329, where zeta_R is
the appropriate root from FIPS 203 Appendix A, Table 2). Use the unoptimized FIPS
spec directly for the first implementation (Alg 4 for NTT, Alg 5 for InvNTT).

#### 3.1.3 CBD (FIPS A.3.2)

eta = 2 for all params. Input: k * n * eta / 4 = k * 128 bytes. Each byte yields 2
coefficients: popcount(low nibble) - popcount(high nibble), range [-4, +4].

Add q to each coefficient, then reduce mod q to ensure [0, q).

### 3.2 mlkem-encode.snd

Packs polynomials into byte arrays. 256 coeffs * 12 bits = 384 bytes per poly.

| Function | Description | Output size |
|----------|-------------|-------------|
| mlkem_encode_poly(out, a) | 12-bit pack, 1 poly | 384 B |
| mlkem_decode(r, a) | 12-bit unpack, 1 poly | N/A |
| mlkem_encode_pk(out, t, k) | k polys | 384k B |
| mlkem_encode_ct(out, u, v, k) | k+1 polys (plain) | 384*(k+1) B |
| mlkem_decode_ct_u(out, u, k) | Decode u vector (compressed) | N/A |
| mlkem_decode_ct_v(out, v, k) | Decode v vector (compressed) | N/A |

Compressed ciphertext (FIPS 203 Appendix A Table 3):

| Component | Encode | Bits/poly | Bytes/poly |
|-----------|--------|-----------|------------|
| u_j (k polys) | compress(10) | 10 | 320 |
| v (1 poly) | decompress-compress(4) | 4 | 128 |

Ciphertext size = 320 * k + 128. Verified: ML-KEM-512: 768, ML-KEM-768: 1088, ML-KEM-1024: 1568.

### 3.3 mlkem-pke.snd: K-PKE Core

| Function | Reference | Description |
|----------|-----------|-------------|
| mlkem_kpke_encrypt(ek_enc, m, r_enc, out_ct, sigma, k) | FIPS Alg 13 | k-PKE.Encrypt |
| mlkem_kpke_decrypt(dk, ct_enc, out_m, out_mu, k) | FIPS Alg 18 | k-PKE.Decrypt |

gen_A: matrix A (k x k polys) generated from seed rho via PRF (FIPS 203 Section 7.2.1,
SHAKE128 with domain separator).

### 3.4 mlkem-kem.snd: Top-Level ML-KEM API

| Function | Reference | Description |
|----------|-----------|-------------|
| mlkem512_keygen(out_pk, out_dk) | Alg 19 | KeyGen (512) |
| mlkem512_encaps(out_ct, out_ss, pk) | Alg 20 | Encaps (512) |
| mlkem512_decaps(out_ss, dk, ct) | Alg 21 | Decaps (512), 0=ok 1=fail |
| mlkem768_keygen(out_pk, out_dk) | Alg 19 | KeyGen (768) |
| mlkem768_encaps(out_ct, out_ss, pk) | Alg 20 | Encaps (768) |
| mlkem768_decaps(out_ss, dk, ct) | Alg 21 | Decaps (768) |
| mlkem1024_keygen(out_pk, out_dk) | Alg 19 | KeyGen (1024) |
| mlkem1024_encaps(out_ct, out_ss, pk) | Alg 20 | Encaps (1024) |
| mlkem1024_decaps(out_ss, dk, ct) | Alg 21 | Decaps (1024) |

KeyGen (Alg 19): seed(64B) -> SHAKE128(seed, 64) -> rho(32), seed'(32). seed'(32) ->
SHAKE256(seed', 64) -> d(32), prf(32). gen_A(rho, k). s = PRF, e = PRF(d). t = A*s + e.
ek = encode(t), dk = (pk_bytes, s, t, rho, seed', d).

Encaps (Alg 20): m = 32 random bytes. (ct, ss) = K-PKE.Encrypt(ek, m, PRF(d, m, 32)).
ss = SHAKE256(m || ss)[:32]. Return (ct, ss).

Decaps (Alg 21): (m, ss) = K-PKE.Decrypt(dk, ct). ct' = K-PKE.Encrypt(ek, m).
If (constant-time) ct != ct': ss = SHAKE256(z, 32).
ss = SHAKE256(m || ss)[:32]. Return ss.

Decaps must be constant-time: no branches on secret data.

---

## 4 FIPS 204: ML-DSA

### 4.1 mldsa-poly.snd

Polynomials: 256 i64 words. Each a[i] in [0, 8380417).

```sond
let MLDSA_Q = 8380417
let MLDSA_ZETA = 17  // primitive 256th root of unity mod q
```

| Function | Description | Reference |
|----------|-------------|-----------|
| mlkem_reduce(a) | Barrett: a mod 8380417 | FIPS 7.1.1 (adapted) |
| mlkem_mont_mul(a, b) | a * b * R^-1 mod q, R=2^15 | FIPS 7.1.1 |
| mlkem_mont_sq(a) | a * a * R^-1 mod q | N/A |
| mlkem_montgomery_reduce(t) | Reduce 2-word to Montgomery form | FIPS 7.1.1 |
| mlkem_poly_add(r, a, b) | r[i] = (a[i]+b[i]) mod q | FIPS 4.3, Alg 2/3 |
| mlkem_poly_sub(r, a, b) | r[i] = (a[i]-b[i]+q) mod q | FIPS 4.3, Alg 2/3 |
| mlkem_poly_compress(r, a, d) | Compress coeff to d bits | FIPS 4.3, Alg 5 |
| mlkem_poly_decompress(r, a, d) | Decompress d-bit coeffs | FIPS 4.3, Alg 6 |
| mlkem_poly_ntt(r, a) | Forward NTT (Montgomery domain) | FIPS 4.4, Alg 20 |
| mlkem_poly_intt(r, a) | Inverse NTT (Montgomery to ring) | FIPS 4.4, Alg 22 |
| mlkem_poly_pointwise(r, a, b) | r[i] = a[i] * b[i] mod q (Montgomery) | N/A |
| mlkem_poly_negate(r, a) | r[i] = (q - a[i]) mod q | N/A |

The Dilithium NTT uses basemul (FIPS 204 Alg 19): a special 2x2 butterfly that
computes [a+b, (a-b)*zeta] in one pass. reducedntt (Alg 20) applies basemul at
each butterfly step.

128 precomputed zetas (symmetric NTT), each = zeta^R * R mod q.

### 4.2 mldsa-sampling.snd

| Function | Description | Reference |
|----------|-------------|-----------|
| mldsa_prf(out, len, seed, nonce) | SHAKE256(seed || nonce)[:len] | FIPS 4.2, Alg 1 |
| mldsa_expand_s(out_s, seed, tau, l, k, eta) | Sample s from seed | FIPS 4.2, Alg 2 (ExpandS) |
| mldsa_try_inverses(rho, pk_enc, sk_enc, k, l, s1, s2, t, tau) | KeyGen inner loop | Section 5 |
| mldsa_expand_mask(out, seed, tau, msg, kappa) | Sample mask, bit 1 of SHAKE | FIPS 4.2, Alg 3 |
| mldsa_gamma1_sample(out, mu, sigma, l, nbits) | Sample from {-(gamma1-1)/2..(gamma1-1)/2} | FIPS 4.3 |

ExpandMask (FIPS 204 Alg 28): consumes output from bit position 1 of the SHAKE
bitstream, not bit 0.

### 4.3 mldsa-pke.snd: PQPKE

Similar to K-PKE but l rows, L columns, different compression parameters, Dilithium NTT.
Implements FIPS 204 Algorithms 7 (Encrypt) and 8 (Decrypt).

### 4.4 mldsa-sig.snd: ML-DSA Signature API

| Function | Reference | Description |
|----------|-----------|-------------|
| mldsa44_keygen(out_pk, out_sk) | Alg 1 | KeyGen (44) |
| mldsa44_sign(out_sig, sk, msg, msg_len) | Alg 2 | Sign (44) |
| mldsa44_verify(pk, msg, msg_len, sig, sig_len) | Alg 3 | Verify (44), 0=ok |

Expanded SK: (seed(32), rho(32), s1[l polynomials], s2[l polynomials],
compressed_t[l * 320], Z(32))

Total sizes from FIPS 204 Tables 1 & 2:
- ML-DSA-44 PK: 1312 B, SK: 2560 B, Sig: 2420 B
- ML-DSA-65 PK: 1952 B, SK: 4032 B, Sig: 3309 B
- ML-DSA-87 PK: 2592 B, SK: 4896 B, Sig: 4627 B

KeyGen (Alg 1) outline:
1. seed -> SHAKE128(seed, 64) -> rho, rho'
2. gen_A: A[l x L] from rho via PRF (Alg 1)
3. s1 = ExpandS(rho', 0, tau, l, eta), s2 = ExpandS(rho', 1, tau, l, eta)
4. t = A * s1 + s2, reject until ||t|| < threshold
5. pk = encode(t), sk = (seed, rho, s1, s2, t, Z)

Sign (Alg 2) outline:
1. mu = SHAKE256(rho_bytes || m, 64)
2. For kappa = 0..999:
   a. y = PRF(Z, kappa, tau, mu) -> samples y from {-(γ1-1)/2..γ1-1}/2}
   b. w = A * y (mod q) [matrix-vector, NTT domain]
   c. w_hat = Encode_L(kappa_i, w) [compress to kappa_i bits]
   d. ct_hat = sample_challenge(w_hat_bytes || mu, kappa, n) [Alg 29]
   e. s1_out = y - ct_hat * s1_sk (mod q)
   f. Check: ||s1_out||_inf < gamma1 - tau, ||s2_out||_inf < gamma2
   g. s2_out = t * ct_hat + s2_sk (mod q) -- [CHECK AGAINST C REFERENCE]
   h. If both shortness checks pass: break

Verify (Alg 3) outline:
1. Check ||s1_out||_inf < gamma1 - tau and ||s2_out||_inf < gamma2. If fail: return FAIL.
2. w' = A * s2_out - t * s1_out (mod q)
3. w'_comp = Encode_L(kappa_i, w')
4. ct_hat_recomp = SHAKE256(w'_comp || mu)[:challenge_bytes]
5. Decode(ct_hat_recomp) == Decode(ct_hat_from_sig) ? PASS : FAIL

The exact byte layout of the signature must be verified against the C reference code.

### 4.5 Signature Encoding (FIPS 204 Section 4.3)

The Encode (Alg 5) and EncodeL functions implement variable-bit packing similar to
ML-KEM but with different bit widths per parameter set:
- s1: compressed with 6 bits per coefficient (z = 6)
  -> 6 * 256 / 8 = 192 bytes per poly
  -> l * 192 bytes total (l = 4/6/8)
- s2: compressed with 12 bits per coefficient (z = 12)
  -> 12 * 256 / 8 = 384 bytes per poly
  -> l * 384 bytes total
- challenge ct_hat: compressed with kappa bits per coefficient
  -> kappa * 256 / 8 bytes (one polynomial)
  -> ML-DSA-44: 224 bytes, ML-DSA-65: 320 bytes, ML-DSA-87: 192 bytes

The total signature size is:
  cHash(32) + ct_hat(kappa * 32) + s1_encoded(l * 192) + s2_encoded(l * 384)
  + possibly additional fields (the exact layout must be verified against the C reference).

---

## 5 shake.snd: SHAKE128 / SHAKE256 XOF (NEW)

Two options:
A. Implement full Keccak-f[1600] permutation from scratch (like chacha20.snd).
B. Use Keccak-p[1600, 24] directly (not yet in Sond's existing crypto).

Recommend option A: Keccak-f[1600] from scratch, sharing one permutation implementation
for both SHAKE128 and SHAKE256.

### 5.1 Keccak Parameters

Keccak-f[1600]: state = 200 bytes (5x5x8), 24 rounds, round constants RC[24].

Parameters:

| Function | Rate (bytes) | Capacity (bytes) | Total (bytes) |
|----------|-------------|------------------|---------------|
| SHAKE128 | 168         | 32               | 200           |
| SHAKE256 | 136         | 64               | 200           |

### 5.2 API

| Function | Description |
|----------|-------------|
| shake_state_init(state) | Initialize sponge state (200 bytes) |
| shake_state_absorb(state, data, len, domain_seph) | Absorb data + domain sep |
| shake_state_squeeze(state, out, outlen) | Squeeze out bytes |
| shake128(data, len, out, outlen) | Convenience: SHAKE128(data)[:outlen] |
| shake256(data, len, out, outlen) | Convenience: SHAKE256(data)[:outlen] |

Domain separators: 0x1F for ML-KEM domain, 0x06 for ML-DSA domain, etc.
The SHAKE domain separator is absorbed AFTER the data and BEFORE squeezing.

### 5.3 Tests

Verify against NIST SHA-3 test vectors (available at NIST).

---

## 6 Known Answer Tests (KAT)

### 6.1 Official Sources

1. NIST ACVP Server (final FIPS, recommended):
   https://github.com/usnistgov/ACVP-Server/tree/master/gen-val/json-files
   Contains JSON files for: ML-KEM-keyGen-FIPS203/, ML-KEM-encapDecap-FIPS203/, and
   corresponding ML-DSA directories.

2. Reference implementation KATs:
   Kyber: https://pq-crystals.org/kyber/ (KAT files download)
   Dilithium: https://pq-crystals.org/dilithium/ (KAT files download)

3. NIST PQC Intermediate Values (historical, draft versions):
   https://csrc.nist.gov/csrc/media/Projects/post-quantum-cryptography/documents/artifacts/PQC-Intermediate-Values-nov2023.zip

### 6.2 Using KATs in Sond Tests

Each file foo_test.snd:
1. include "<module>.snd" and "x86_64/linux/testing.snd"
2. Load KAT vectors as module-level hex string constants
3. For each KAT entry: decode hex -> call the function -> assert_eq(output, expected)
4. At minimum: test all 3 parameter sets for each algo function.
5. Roundtrip: keygen -> encrypt -> decrypt (recover), sign -> verify (pass).

### 6.3 Reference Code as Secondary Oracle

Source: https://github.com/pq-crystals/kyber and https://github.com/pq-crystals/dilithium
(public domain). Use to verify intermediate values (NTT outputs, CBD outputs, matrix-mul
results) and edge cases (all-zeros, all-ones).

---

## 7 Implementation Challenges

### 7.1 Constant-Time Execution

Critical for Decaps (ML-KEM) and Verify (ML-DSA) and PQPKE Decaps.

Sond achieves constant-time behavior by:
- Avoiding branches on secret data: use if/else expressions where both branches execute.
- Using the OR-of-XOR-diffs pattern (as in chacha20poly1305.snd) for byte comparison.
- Ensuring loop bounds don't depend on secret values.

### 7.2 Division by Constants Only

Sond's `/` operator performs integer division. Crypto modules must only divide by constants.
Barrett reduction uses multiplication and shift only (no division).
Compress: ((x << d) + q/2) / q -- q is constant, safe.

### 7.3 Sampling

**CBD:** Bit extraction from byte arrays: peek8 + & and >>.
**Rejection sample:** While loop with fixed upper bound (e.g., 32 attempts).
**Challenge sample:** Consume kappa * n / 8 bytes from SHAKE, interpret as little-endian
signed values (Alg 29, FIPS 204, for ML-DSA).

### 7.4 Modular Arithmetic Fits in i64

ML-KEM: 3329^2 * 256 = 2.8 x 10^9 < 2^32. No overflow in any intermediate.
ML-DSA: 8380417^2 * 256 = 1.8 x 10^16 < 2^63 (i64 max 9.2 x 10^18). Safe!

### 7.5 Stack Space

Each polynomial = 256 * 8 = 2048 bytes. A matrix (k x k) = k^2 * 2048 bytes.
ML-KEM-1024 (k=4): 16 * 2048 = 32768 bytes just for A.

Use module-level static buffers for large reusable tables (like sha512 K[]).
Functions allocate their own stack arrays.

### 7.6 12-bit Packing

Each ML-KEM coeff in [0, 3328]. Pack 2 per 3 bytes:
```
byte0 = coeff0 & 0xFF
byte1 = (coeff0 >> 8) | ((coeff1 & 0x0F) << 4)
byte2 = coeff1 >> 4
```
256 coeffs = 384 bytes. Compressed: 320 bytes (10-bit) or 128 bytes (4-bit).

---

## 8 Module Implementation Checklist

### shake.snd
- [ ] Keccak-f[1600] permutation (24 rounds, RC[24] constants)
- [ ] Absorb (rate 168/136 bytes), squeeze
- [ ] shake128 and shake256 convenience functions
- [ ] Test against NIST SHA-3 vectors

### mlkem-poly.snd
- [ ] mlkem_reduce (Barrett)
- [ ] Montgomery multiplication chain (reduce, mul, sq)
- [ ] MLKEM_ZETAS[128] (FIPS A Table 2)
- [ ] NTT / InvNTT (unoptimized FIPS spec)
- [ ] pointwise, add, sub, cbd (eta=2), compress/decompress
- [ ] Test: NTT roundtrip cbd against reference, compare KAT

### mlkem-encode.snd
- [ ] 12-bit pack/unpack
- [ ] Compressed 10-bit / 4-bit pack/unpack
- [ ] Matrix A encoding (k^2 polynomials)
- [ ] Test: encode/decode roundtrip

### mlkem-pke.snd
- [ ] gen_A from rho via PRF
- [ ] mlkem_kpke_encrypt (Alg 13)
- [ ] mlkem_kpke_decrypt (Alg 18)
- [ ] Test: encrypt/decrypt roundtrip (KAT)

### mlkem-kem.snd
- [ ] KeyGen (Alg 19)
- [ ] Encaps (Alg 20)
- [ ] Decaps (Alg 21), constant-time
- [ ] Test: all 3 parameter sets against KAT files

### mldsa-poly.snd
- [ ] Basemul (FIPS Alg 19), reducedntt (Alg 20), full ntt/invntt (Alg 22)
- [ ] Pointwise mul, add, sub (Montgomery form)
- [ ] Compress/decompress (Dilithium variant)
- [ ] Test: NTT roundtrip, compare NTT output against Dilithium reference

### mldsa-sampling.snd
- [ ] PRF (SHAKE256-based)
- [ ] ExpandS (deterministic sampling)
- [ ] ExpandMask (bit 1 of SHAKE)
- [ ] Rejection sampling for gamma1 range
- [ ] Test: compare against Dilithium reference C code

### mldsa-pke.snd
- [ ] PQPKE.Encrypt (Alg 7)
- [ ] PQPKE.Decrypt (Alg 8)
- [ ] Matrix-vector multiply (NTT domain, l x L dimensions)

### mldsa-sig.snd
- [ ] KeyGen (Alg 1)
- [ ] Sign (Alg 2)
- [ ] Verify (Alg 3)
- [ ] Test: all 3 parameter sets against KAT files

### * _test.snd (for each module)
- [ ] Include module + testing.snd
- [ ] Load KAT vectors as hex string constants
- [ ] assert_eq for each KAT entry
- [ ] Roundtrip tests, edge cases

---

## 9 References

| Resource | Link |
|----------|------|
| FIPS 203 (ML-KEM PDF) | https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.203.pdf |
| FIPS 204 (ML-DSA PDF) | https://nvlpubs.nist.gov/nistpubs/fips/nist.fips.204.pdf |
| RFC 9936 (ML-KEM CMS) | https://www.rfc-editor.org/rfc/rfc9936.html |
| RFC 9881 (ML-DSA X.509) | https://www.rfc-editor.org/rfc/rfc9881.html |
| NIST ACVP KAT JSON | https://github.com/usnistgov/ACVP-Server/tree/master/gen-val/json-files |
| Kyber KAT / reference | https://pq-crystals.org/kyber/ |
| Dilithium KAT / reference | https://pq-crystals.org/dilithium/ |
| PQC Intermediate Values (draft) | https://csrc.nist.gov/csrc/media/Projects/post-quantum-cryptography/documents/artifacts/PQC-Intermediate-Values-nov2023.zip |
