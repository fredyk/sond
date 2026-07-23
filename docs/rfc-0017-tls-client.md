# RFC-0017: A TLS 1.3 client in Sond, over the existing crypto primitives

| Field       | Value                                                                 |
|-------------|-----------------------------------------------------------------------|
| RFC         | 0017                                                                  |
| Status      | Draft (design; record layer + handshake buildable now, public-CA auth gated on new primitives) |
| Date        | 2026-07-23                                                           |
| Authors     | Jhon Magdalena Vila                                                         |
| Motivation  | Talk to an HTTPS upstream without an external TLS-terminating sidecar: the program speaks TLS itself, in Sond |
| Related     | RFC-0000 (self-hosted tooling), the `any/crypto` primitives, `x86_64/linux/net.snd`, `x86_64/linux/io.snd` |

## 1. The thesis

A program that already carries its own crypto should be able to open an authenticated, encrypted
connection without a `socat`/`stunnel` sidecar in front of it. TLS 1.3 (RFC 8446) is a good fit for a
verifiability-first language: one round trip, a small mandatory feature set, AEAD-only records, and a
key schedule that is a fixed HKDF ladder. This RFC designs a TLS 1.3 CLIENT in Sond, built on the
primitives already in `any/crypto`, and states honestly which part lands now and which part is gated
on primitives the tree does not yet have.

## 2. What the tree already has (and what it does not)

Present in `any/crypto`, enough for the record layer, the key schedule, and the handshake transcript:

- `aead/chacha20poly1305.snd`: `aead_seal(key, nonce, aad, alen, pt, n, ct, tag)` / `aead_open(...)`,
  the record-protection AEAD for the `TLS_CHACHA20_POLY1305_SHA256` cipher suite.
- `dh/x25519.snd`: `x25519(scalar_ptr, u_ptr, out_ptr)`, the `x25519` named group for the key share.
- `kdf/hkdf-sha256.snd`: `hkdf_extract` / `hkdf_expand` / `hmac_sha256`, the whole key schedule.
- `hash/sha256.snd`: `sha256_hash(msg, len, out)`, the transcript hash and the HKDF hash.

NOT present, and this is the crux of authentication:

- No RSA (PKCS#1 v1.5 / PSS) and no ECDSA over NIST P-256. Real public certificates are signed with
  one of these, essentially never with ed25519. We have `sign/ed25519-sign.snd`, which authenticates
  ed25519 peers but not a public CA chain.
- No X.509 / ASN.1 DER parser: the certificate message is a DER blob we cannot currently walk.
- No AES-GCM. TLS 1.3 lists `TLS_AES_128_GCM_SHA256` as mandatory-to-implement, but a CLIENT may
  simply offer only `TLS_CHACHA20_POLY1305_SHA256`; the widely deployed servers (Google, Cloudflare,
  and anything behind them) support it. We offer the one suite we can build.

The consequence is not cosmetic and must not be papered over: **the record layer and the full
handshake up to `Finished` are buildable now, but authenticating an arbitrary public HTTPS server is
not**, because authentication requires verifying the server's `CertificateVerify` signature with the
certificate's own key type (RSA or ECDSA-P256) and parsing the certificate chain. Pinning the leaf
certificate does NOT dodge this: the pinned certificate is public, so a man in the middle can present
it; only `CertificateVerify` (the server signing the transcript with the matching private key) proves
the peer holds that key, and verifying it needs the missing signature primitive. So there is no secure
shortcut to public-CA authentication with the primitives on hand. This RFC says so plainly rather than
shipping a client that looks encrypted but is not authenticated.

## 3. The two tiers

The work splits cleanly, and the split is by AUTHENTICATION, not by transport (the record layer and
handshake framing are shared):

### Tier A: buildable now, authenticated

The complete TLS 1.3 record layer, handshake, and key schedule, authenticated in one of the modes we
CAN verify with current primitives:

- **PSK mode (RFC 8446 §2.2).** A pre-shared key authenticates the handshake through the PSK binder
  (an HMAC over the transcript), with NO certificate at all. Uses only HKDF / HMAC / SHA-256, all
  present. This is the right mode for our own service-to-service links (e.g. the proxy to an edge we
  also control), where both ends can hold a shared secret.
- **ed25519 peer (certificate or Raw Public Key, RFC 7250) mode.** When the peer authenticates with an
  ed25519 key, `CertificateVerify` is an ed25519 signature we can verify with `ed25519-sign.snd`,
  after pinning the expected public key. Suitable for a controlled upstream configured with an ed25519
  identity.

Tier A is a real, secure, self-contained deliverable and is the first implementation target.

### Tier B: public-CA HTTPS, gated on new primitives

To authenticate an arbitrary public server (the LLM upstream is one), Tier B needs, as new work:

1. **ECDSA over NIST P-256 verification** (P-256 field + group arithmetic + the ECDSA verify
   equation). Most modern certificates are ECDSA-P256 or RSA; P-256 is the smaller lift and covers a
   large share.
2. **RSA PKCS#1 v1.5 verification** (a bignum `modexp` with the public exponent) for the RSA share of
   the web.
3. **A minimal X.509 / ASN.1 DER reader**: walk the certificate to reach the SubjectPublicKeyInfo, the
   signature, the validity dates, and the SAN (to match the hostname), plus chain building to a
   trusted root set.

Each is its own effort (and arguably its own RFC). Until they exist, a Sond program CANNOT securely
originate TLS to a public CA endpoint; the honest fallbacks are (a) keep a TLS-terminating sidecar in
front of the public upstream, or (b) route through an edge we control and speak Tier A (PSK / ed25519)
to that edge, letting it do the public-internet TLS. This RFC does not pretend Tier B is done.

## 4. The record layer (Tier A and B share this)

A TLS record is a 5-byte header `{ ContentType(1), legacy_version(2)=0x0303, length(2) }` followed by
the fragment. After the handshake keys are installed, every record is a `TLSCiphertext`: the plaintext
is `inner = content || ContentType || zeros?`, sealed with the AEAD into `ct || tag`, and the record's
`ContentType` on the wire is `application_data(23)`.

- **Nonce (RFC 8446 §5.3).** Per key direction we hold a 12-byte `iv` and a 64-bit sequence number
  starting at 0. The record nonce is `iv XOR (seq, right-aligned, big-endian)`; `seq` increments per
  record and resets to 0 whenever the key changes (handshake to application). `aead_seal`/`aead_open`
  take the nonce directly.
- **AAD.** The 5-byte record header of the `TLSCiphertext` is the AEAD associated data.
- **API sketch** (buffers caller-provided, in the no-allocator style of `net.snd`/`io.snd`):
  ```
  fn tls_record_send(fd, keys, content_type, buf, n) -> i64      // seal + write one record
  fn tls_record_recv(fd, keys, out, cap) -> i64                  // read + open one record, returns (content_type<<32)|len
  ```
  where `keys` is a small struct/region holding `{ key[32], iv[12], seq }` for one direction.

## 5. The key schedule (RFC 8446 §7.1), all HKDF

A direct transcription of the ladder onto `hkdf_extract` / `hkdf_expand` / `sha256_hash`. The one
helper to write is `HKDF-Expand-Label`:

```
HKDF-Expand-Label(secret, label, context, len):
    info = u16(len) || u8(len("tls13 "+label)) || "tls13 "+label || u8(len(context)) || context
    return hkdf_expand(secret, info, len)
Derive-Secret(secret, label, transcript_msgs):
    return HKDF-Expand-Label(secret, label, sha256(transcript_msgs), 32)
```

The stages: `early_secret = hkdf_extract(0, psk_or_0)`; `handshake_secret = hkdf_extract(Derive-Secret(
early, "derived", ""), ecdhe)` where `ecdhe = x25519(our_priv, server_key_share)`; then the
`client/server handshake_traffic_secret`s, the record keys via `HKDF-Expand-Label(secret, "key"/"iv")`,
and after `Finished`, `master_secret` and the `application_traffic_secret`s. `Finished` is
`HMAC(finished_key, sha256(transcript))`, which we verify (server) and produce (client) with
`hmac_sha256`.

## 6. The handshake flow (client)

1. Generate an x25519 keypair (`x25519(priv, base=9, pub)`); random 32-byte `ClientHello.random`.
2. Send `ClientHello` with extensions: `supported_versions=[TLS1.3]`, `supported_groups=[x25519]`,
   `key_share=[x25519: pub]`, `signature_algorithms` (advertising what Tier we can verify),
   `server_name` (SNI), and cipher suite `TLS_CHACHA20_POLY1305_SHA256`.
3. Read `ServerHello`, take the server `key_share`, compute `ecdhe`, run the key schedule to the
   handshake traffic keys, install them on the record layer.
4. Decrypt `EncryptedExtensions`, then depending on tier: PSK mode has no certificate; a certificate
   mode reads `Certificate` + `CertificateVerify` and AUTHENTICATES (Tier A: ed25519 verify against a
   pinned key; Tier B: chain + P-256/RSA verify, once those exist).
5. Verify the server `Finished`; send the client `Finished`; switch to the application traffic keys.
6. `application_data` records now carry the HTTP request/response (RFC-0007 FFI or a Sond HTTP client
   on top).

Each incoming message is folded into the running transcript hash (an incremental SHA-256 would avoid
re-hashing; the first cut may re-hash the accumulated transcript buffer, since handshakes are small).

## 7. Testing (TDD, per the project discipline)

TLS has canonical test vectors, so the ladder is testable without a network:

- **RFC 8448** ("Example Handshake Traces for TLS 1.3") gives byte-exact transcripts and every derived
  secret. The key schedule and record protection are unit-tested against it: feed the RFC 8448
  `ClientHello`/`ServerHello`/shared secret, assert each `Derive-Secret` output and the record keys
  match the document byte for byte. This is the red-first anchor for §5.
- **Record layer**: seal a known plaintext with a known key/nonce, assert the ciphertext+tag, then
  `open` it back (round trip), plus a tamper test (a flipped tag byte must make `aead_open` fail).
- **Handshake**: a Sond in-process fake peer that replays an RFC 8448 server flight lets the client
  handshake be driven end to end with no socket, asserting the client reaches the application keys.
- **Live smoke** (integration, not in the gate): a real TLS 1.3 server that supports
  ChaCha20-Poly1305, behind Tier A auth, to confirm the socket path.

## 8. Scope and non-goals (v1)

In: TLS 1.3 only; client only; `TLS_CHACHA20_POLY1305_SHA256`; `x25519`; PSK and ed25519-peer
authentication; the record layer, key schedule, and handshake to application data.

Out (documented, not hidden): TLS 1.2; server side; AES-GCM; session resumption / 0-RTT; the public-CA
authentication of Tier B (ECDSA-P256 / RSA verify + X.509/ASN.1), which is the gating follow-up before
a Sond program can originate TLS to an arbitrary public HTTPS endpoint.

## 9. Decision this RFC asks for

Confirm the phasing: build **Tier A** now (record layer + key schedule + handshake + PSK/ed25519 auth,
fully testable against RFC 8448), and treat **Tier B** (P-256/RSA verify + X.509) as a separately
scoped effort. This keeps every shipped step honest: what lands is genuinely encrypted AND
authenticated, and the public-CA gap is stated in the open rather than shipped as a silent
"encrypted-but-unauthenticated" client.
