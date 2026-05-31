//! `node:crypto` — hashing, HMAC, symmetric ciphers, key derivation, and CSPRNG primitives.
//!
//! ## Architecture
//!
//! The cryptographic heart of the module is a **pure-Rust core** that never touches Nova:
//!
//! * [`Algorithm`] — the supported digest algorithms (`sha256`, `sha1`, `md5`, plus the `sha2`
//!   siblings `sha224`/`sha384`/`sha512` which come free with the `sha2` crate).
//! * [`digest`] — one-shot `digest(alg, data) -> Vec<u8>`.
//! * [`hmac`] — `HMAC(alg, key, data) -> Vec<u8>`.
//! * [`cipher_encrypt`] / [`cipher_decrypt`] — AES-128/256-CBC with PKCS#7 padding (RustCrypto
//!   `aes` + `cbc`), behind `createCipheriv`/`createDecipheriv`.
//! * [`pbkdf2`] — PBKDF2-HMAC key derivation (RustCrypto `pbkdf2` over the same `hmac`/`sha2`),
//!   behind `pbkdf2`/`pbkdf2Sync`.
//! * [`random_bytes`] / [`random_uuid`] — the OS CSPRNG (`getrandom`) behind `randomBytes`,
//!   `randomFillSync`, and a RFC 4122 v4 `randomUUID`.
//!
//! This core is exhaustively unit-tested in isolation (no JS agent needed) — tenet 1 (no `unsafe`)
//! and tenet 3 (the digest/cipher paths borrow their input `&[u8]`; the only allocation is the
//! unavoidable output `Vec<u8>` / `String`).
//!
//! The JS-facing surface is a thin layer over that core. A handful of Rust-backed
//! [`nova_vm::ecmascript::RegularFn`] **natives** marshal bytes in and encoded digests / ciphertext /
//! random bytes out, and a compile-time JS **prelude** assembles the Node `createHash().update()
//! .digest()` object graph, `createHmac`, `createCipheriv`/`createDecipheriv`, `pbkdf2`/`pbkdf2Sync`,
//! `randomBytes`, `randomUUID`, `randomFillSync`, and `timingSafeEqual` over them — exactly how the
//! Buffer module layers its JS API over Rust codecs.
//!
//! ## Why incremental `Hash`/`Cipheriv` accumulate JS-side
//!
//! A Nova builtin function is a bare `fn` pointer with no captured state, so it cannot hold a live
//! Rust streaming hasher/cipher between `.update()` calls. A digest is content-addressable
//! (`digest === H(chunk0 ++ chunk1 ++ …)`) and a CBC ciphertext is a pure function of the whole
//! padded input, so the `Hash`/`Cipheriv` JS objects collect each `update` chunk and the result is
//! computed in a single native call over the concatenation — byte-for-byte identical to a streaming
//! form, the difference being purely where the bytes are buffered. This is the same "JS object graph
//! over Rust hot-path natives" split the `node:buffer` module uses.
//!
//! ## Documented gaps
//!
//! `scrypt`/`scryptSync` need a pure-Rust scrypt crate that is not vendored offline, so they throw a
//! clear, catchable "not implemented" `Error` rather than returning wrong bytes; they are a tracked
//! follow-up. AES-GCM / other modes and asymmetric crypto are likewise out of scope for this offline
//! surface.
//!
//! ## Laziness
//!
//! Built at most once, on the first `require("node:crypto")` / `import` (tenet 2). Until then it
//! costs one `&'static str` table entry; the prelude is a `&'static str`, so an un-imported runtime
//! pays nothing for it.

use aes::{Aes128, Aes256};
use cbc::{Decryptor, Encryptor};
use cipher::block_padding::Pkcs7;
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use digest::Digest;
use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Sha224, Sha256, Sha384, Sha512};

use nova_vm::ecmascript::{
    Agent, Array, ArgumentsList, ArrayBuffer, ExceptionType, InternalMethods, JsError, JsResult,
    Object, OrdinaryObject, PropertyDescriptor, PropertyKey, String as JsString, TypedArray, Value,
    parse_script, script_evaluation,
};
use nova_vm::engine::{Bindable, GcScope, NoGcScope};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::globals::define_fn;
use crate::node::NodeModule;

// =================================================================================================
// Pure-Rust crypto core (no Nova; unit-tested directly).
// =================================================================================================

/// A supported message-digest algorithm.
///
/// Node names algorithms case-insensitively and tolerates a few aliases (`sha-256`, `ssl3-md5`-era
/// short forms are out of scope). The set here covers the common SHA-2 family, SHA-1, and MD5 — the
/// algorithms requested for `createHash`/`createHmac` plus the SHA-2 siblings that the `sha2` crate
/// supplies at no extra cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Algorithm {
    Md5,
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl Algorithm {
    /// Resolve a Node algorithm name (case-insensitive, tolerating a `sha-` separator) to an
    /// [`Algorithm`], or `None` if unsupported here.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        // Normalize: lowercase and drop any single `-` inside the SHA family (`sha-256` -> `sha256`).
        let lower = name.trim().to_ascii_lowercase();
        let normalized = lower.replace("sha-", "sha");
        match normalized.as_str() {
            "md5" => Some(Algorithm::Md5),
            "sha1" => Some(Algorithm::Sha1),
            "sha224" => Some(Algorithm::Sha224),
            "sha256" => Some(Algorithm::Sha256),
            "sha384" => Some(Algorithm::Sha384),
            "sha512" => Some(Algorithm::Sha512),
            _ => None,
        }
    }

    /// The digest output length in bytes.
    pub(crate) fn output_len(self) -> usize {
        match self {
            Algorithm::Md5 => 16,
            Algorithm::Sha1 => 20,
            Algorithm::Sha224 => 28,
            Algorithm::Sha256 => 32,
            Algorithm::Sha384 => 48,
            Algorithm::Sha512 => 64,
        }
    }
}

/// One-shot message digest: `digest(alg, data) -> Vec<u8>`.
///
/// Borrows the input bytes (tenet 3); the only allocation is the fixed-size output `Vec`. Each arm
/// drives the corresponding `Digest` implementation from its crate — all pure-Rust, no OpenSSL.
pub(crate) fn digest(alg: Algorithm, data: &[u8]) -> Vec<u8> {
    match alg {
        Algorithm::Md5 => Md5::digest(data).to_vec(),
        Algorithm::Sha1 => Sha1::digest(data).to_vec(),
        Algorithm::Sha224 => Sha224::digest(data).to_vec(),
        Algorithm::Sha256 => Sha256::digest(data).to_vec(),
        Algorithm::Sha384 => Sha384::digest(data).to_vec(),
        Algorithm::Sha512 => Sha512::digest(data).to_vec(),
    }
}

/// Keyed-hash message authentication code: `HMAC(alg, key, data) -> Vec<u8>`.
///
/// Uses the RFC 2104 `hmac` construction over the selected digest. `Hmac::new_from_slice` accepts a
/// key of any length (it is hashed/padded to the block size per the spec), so this never fails for a
/// supported algorithm.
pub(crate) fn hmac(alg: Algorithm, key: &[u8], data: &[u8]) -> Vec<u8> {
    // Monomorphized per algorithm: `Hmac<D>` carries a thicket of `digest`-internal bounds that are
    // far simpler to satisfy by naming each concrete digest type than by writing a generic helper.
    macro_rules! run {
        ($d:ty) => {{
            let mut mac =
                <Hmac<$d> as Mac>::new_from_slice(key).expect("HMAC accepts a key of any length");
            mac.update(data);
            mac.finalize().into_bytes().to_vec()
        }};
    }
    match alg {
        Algorithm::Md5 => run!(Md5),
        Algorithm::Sha1 => run!(Sha1),
        Algorithm::Sha224 => run!(Sha224),
        Algorithm::Sha256 => run!(Sha256),
        Algorithm::Sha384 => run!(Sha384),
        Algorithm::Sha512 => run!(Sha512),
    }
}

/// Fill `out` with cryptographically secure random bytes from the OS CSPRNG.
///
/// `getrandom` is the same source Node's `crypto.randomFillSync` ultimately draws from. On the rare
/// platform error this returns `Err`; callers surface it as a JS `Error`.
pub(crate) fn random_fill(out: &mut [u8]) -> Result<(), getrandom::Error> {
    getrandom::getrandom(out)
}

/// Allocate `n` cryptographically secure random bytes.
pub(crate) fn random_bytes(n: usize) -> Result<Vec<u8>, getrandom::Error> {
    let mut buf = vec![0u8; n];
    random_fill(&mut buf)?;
    Ok(buf)
}

/// Generate an RFC 4122 version-4 (random) UUID string, e.g. `36b8f84d-df4e-4d49-b662-bbb1c1e1c1c1`.
///
/// 16 CSPRNG bytes with the version nibble pinned to `4` and the variant bits to `10`, formatted
/// lowercase hyphenated — byte-for-byte the shape Node's `crypto.randomUUID()` returns.
pub(crate) fn random_uuid() -> Result<String, getrandom::Error> {
    let mut b = [0u8; 16];
    random_fill(&mut b)?;
    // Version 4: high nibble of byte 6 = 0100.
    b[6] = (b[6] & 0x0f) | 0x40;
    // Variant 10xx: top two bits of byte 8.
    b[8] = (b[8] & 0x3f) | 0x80;
    Ok(format_uuid(&b))
}

// ----- symmetric ciphers (AES-CBC) and key derivation (PBKDF2) -------------------------------

/// A supported symmetric cipher transform. The CBC mode pads with PKCS#7 — the default Node applies
/// for `createCipheriv("aes-256-cbc", …)` (block ciphers require padding for a full-block output).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Cipher {
    /// `aes-128-cbc`: 16-byte key, 16-byte IV.
    Aes128Cbc,
    /// `aes-256-cbc`: 32-byte key, 16-byte IV.
    Aes256Cbc,
}

impl Cipher {
    /// Resolve a Node cipher name (case-insensitive) to a [`Cipher`], or `None` if unsupported here.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "aes-128-cbc" | "aes128" => Some(Cipher::Aes128Cbc),
            "aes-256-cbc" | "aes256" => Some(Cipher::Aes256Cbc),
            _ => None,
        }
    }

    /// The required key length in bytes.
    pub(crate) fn key_len(self) -> usize {
        match self {
            Cipher::Aes128Cbc => 16,
            Cipher::Aes256Cbc => 32,
        }
    }

    /// The required IV length in bytes (the AES block size for CBC).
    pub(crate) fn iv_len(self) -> usize {
        16
    }
}

/// A cipher operation failure: a wrong-length key/IV, or undecryptable / unpadded ciphertext.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CipherError {
    /// The key or IV did not match the cipher's required length.
    BadKeyOrIv,
    /// Decryption failed (bad padding / corrupt ciphertext / wrong key).
    BadData,
}

/// AES-CBC encrypt `data` with PKCS#7 padding, returning the ciphertext.
///
/// Validates key/IV length up front (Node throws `RangeError` for a mismatch). The single allocation
/// is the output `Vec`; no `unsafe` (tenet 1, tenet 3).
pub(crate) fn cipher_encrypt(
    cipher: Cipher,
    key: &[u8],
    iv: &[u8],
    data: &[u8],
) -> Result<Vec<u8>, CipherError> {
    if key.len() != cipher.key_len() || iv.len() != cipher.iv_len() {
        return Err(CipherError::BadKeyOrIv);
    }
    let out = match cipher {
        Cipher::Aes128Cbc => {
            Encryptor::<Aes128>::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
        }
        Cipher::Aes256Cbc => {
            Encryptor::<Aes256>::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
        }
    };
    Ok(out)
}

/// AES-CBC decrypt `data` (PKCS#7-padded), returning the plaintext.
///
/// Returns [`CipherError::BadData`] for ciphertext that is not a whole number of blocks or whose
/// padding does not validate (a wrong key surfaces here) — matching Node's thrown decrypt error.
pub(crate) fn cipher_decrypt(
    cipher: Cipher,
    key: &[u8],
    iv: &[u8],
    data: &[u8],
) -> Result<Vec<u8>, CipherError> {
    if key.len() != cipher.key_len() || iv.len() != cipher.iv_len() {
        return Err(CipherError::BadKeyOrIv);
    }
    let out = match cipher {
        Cipher::Aes128Cbc => Decryptor::<Aes128>::new(key.into(), iv.into())
            .decrypt_padded_vec_mut::<Pkcs7>(data)
            .map_err(|_| CipherError::BadData)?,
        Cipher::Aes256Cbc => Decryptor::<Aes256>::new(key.into(), iv.into())
            .decrypt_padded_vec_mut::<Pkcs7>(data)
            .map_err(|_| CipherError::BadData)?,
    };
    Ok(out)
}

/// PBKDF2 over HMAC, deriving a `keylen`-byte key.
///
/// `crypto.pbkdf2`/`pbkdf2Sync` accept a `digest` name selecting the underlying HMAC. The supported
/// digests reuse [`Algorithm`]. The single allocation is the output `Vec` (tenet 3).
pub(crate) fn pbkdf2(
    alg: Algorithm,
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    keylen: usize,
) -> Vec<u8> {
    let mut out = vec![0u8; keylen];
    // Monomorphized per digest, mirroring `hmac` above: `pbkdf2_hmac::<D>` is generic over the digest.
    match alg {
        Algorithm::Md5 => pbkdf2::pbkdf2_hmac::<Md5>(password, salt, iterations, &mut out),
        Algorithm::Sha1 => pbkdf2::pbkdf2_hmac::<Sha1>(password, salt, iterations, &mut out),
        Algorithm::Sha224 => pbkdf2::pbkdf2_hmac::<Sha224>(password, salt, iterations, &mut out),
        Algorithm::Sha256 => pbkdf2::pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut out),
        Algorithm::Sha384 => pbkdf2::pbkdf2_hmac::<Sha384>(password, salt, iterations, &mut out),
        Algorithm::Sha512 => pbkdf2::pbkdf2_hmac::<Sha512>(password, salt, iterations, &mut out),
    }
    out
}

/// Format 16 bytes as a lowercase hyphenated RFC 4122 UUID string (`8-4-4-4-12`).
fn format_uuid(b: &[u8; 16]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    // 32 hex digits + 4 hyphens.
    let mut s = String::with_capacity(36);
    for (i, byte) in b.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push(HEX[(byte >> 4) as usize] as char);
        s.push(HEX[(byte & 0x0f) as usize] as char);
    }
    s
}

/// Lowercase hex-encode a byte slice.
fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Standard (RFC 4648 §4) base64-encode a byte slice, with `=` padding — Node's `digest('base64')`.
fn to_base64(bytes: &[u8]) -> String {
    const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(STD[((n >> 18) & 0x3f) as usize] as char);
        out.push(STD[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(STD[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(STD[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// URL-safe (RFC 4648 §5) base64, no padding — Node's `digest('base64url')`.
fn to_base64url(bytes: &[u8]) -> String {
    const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(URL[((n >> 18) & 0x3f) as usize] as char);
        out.push(URL[((n >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(URL[((n >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(URL[(n & 0x3f) as usize] as char);
        }
    }
    out
}

/// Latin-1 ("binary") encode a byte slice: each byte maps to the matching U+00xx code point. This is
/// the bytes-as-string form Node returns for `digest('binary')` / `digest('latin1')`.
fn to_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|&b| b as char).collect()
}

/// Encode a digest according to a Node digest-encoding name. Unknown names fall back to `hex`
/// (the most common default callers reach for); the JS prelude restricts the set it passes here.
fn encode_digest(bytes: &[u8], encoding: &str) -> String {
    match encoding {
        "hex" => to_hex(bytes),
        "base64" => to_base64(bytes),
        "base64url" => to_base64url(bytes),
        "latin1" | "binary" => to_latin1(bytes),
        _ => to_hex(bytes),
    }
}

// =================================================================================================
// JS-facing wiring.
// =================================================================================================

/// Zero-sized marker for the `node:crypto` builtin.
pub(crate) struct CryptoModule;

impl NodeModule for CryptoModule {
    const SPECIFIER: &'static str = "crypto";

    fn build<'gc>(
        agent: &mut Agent,
        ctx: &NodeCtx,
        gc: GcScope<'gc, '_>,
    ) -> Result<Object<'gc>, InstallError> {
        install(agent, ctx, gc)
    }
}

/// The private global key under which the Rust-backed crypto natives are handed to the prelude.
///
/// Installed just before the prelude runs and deleted immediately after, so it never leaks into a
/// user-visible global. Chosen to be collision-proof with any real Node/user global.
const NATIVES_KEY: &str = "__treaty_crypto_natives__";

/// Uniform per-module entry. Returns the `node:crypto` exports object.
///
/// Steps mirror `node:buffer`: (1) build the natives object with the Rust-backed digest / HMAC /
/// cipher / PBKDF2 / CSPRNG primitives, (2) stash it on the realm global under a private key, (3)
/// evaluate the JS prelude (an IIFE that builds and returns the exports object over those natives),
/// (4) delete the private key, (5) return the exports object.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    // (1) Build the natives object.
    let natives = {
        let nogc = gc.nogc();
        let natives = OrdinaryObject::create_empty_object(agent, nogc);
        define_fn(agent, natives, "digestEncoded", native_digest_encoded, 3, nogc);
        define_fn(agent, natives, "digestBytes", native_digest_bytes, 2, nogc);
        define_fn(agent, natives, "hmacEncoded", native_hmac_encoded, 4, nogc);
        define_fn(agent, natives, "hmacBytes", native_hmac_bytes, 3, nogc);
        define_fn(agent, natives, "randomFill", native_random_fill, 1, nogc);
        define_fn(agent, natives, "randomUuid", native_random_uuid, 0, nogc);
        define_fn(agent, natives, "supports", native_supports, 1, nogc);
        define_fn(agent, natives, "cipher", native_cipher, 4, nogc);
        define_fn(agent, natives, "decipher", native_decipher, 4, nogc);
        define_fn(agent, natives, "pbkdf2", native_pbkdf2, 5, nogc);
        natives
    };

    // (2) Stash on the realm global under the private key.
    let global = agent.current_realm(gc.nogc()).global_object(agent);
    {
        let nogc = gc.nogc();
        let key = PropertyKey::from_static_str(agent, NATIVES_KEY, nogc);
        let defined = global.unbind().try_define_own_property(
            agent,
            key.unbind(),
            PropertyDescriptor::new_data_descriptor(natives),
            None,
            nogc,
        );
        if defined.is_break() {
            return Err(InstallError::Nova(
                "could not stash crypto natives on the global".to_owned(),
            ));
        }
    }

    // (3) Evaluate the prelude; its completion value is the exports object.
    let exports = run_prelude(agent, gc.reborrow())?.unbind();

    // (4) Delete the private key so it never leaks to user code.
    {
        let nogc = gc.nogc();
        let global = agent.current_realm(nogc).global_object(agent);
        let key = PropertyKey::from_static_str(agent, NATIVES_KEY, nogc);
        let _ = global.unbind().try_delete(agent, key.unbind(), nogc);
    }

    Ok(exports.bind(gc.into_nogc()))
}

/// Parse + evaluate [`PRELUDE`] in the current realm and return its completion value as an [`Object`].
fn run_prelude<'gc>(
    agent: &mut Agent,
    mut gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let source = JsString::from_static_str(agent, PRELUDE, gc.nogc());
    let realm = agent.current_realm(gc.nogc());
    let script = parse_script(agent, source.unbind(), realm.unbind(), true, None, gc.nogc())
        .map_err(|diags| {
            let msg = diags
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            InstallError::Nova(format!("crypto prelude parse error: {msg}"))
        })?;

    let value = script_evaluation(agent, script.unbind(), gc.reborrow())
        .unbind()
        .bind(gc.nogc());
    let value = match value {
        Ok(v) => v,
        Err(err) => {
            let msg = err
                .value()
                .unbind()
                .string_repr(agent, gc.reborrow())
                .to_string_lossy(agent)
                .into_owned();
            return Err(InstallError::Nova(format!("crypto prelude threw: {msg}")));
        }
    };

    Object::try_from(value.unbind())
        .map(|o| o.unbind().bind(gc.into_nogc()))
        .map_err(|_| InstallError::Nova("crypto prelude did not return an object".to_owned()))
}

// ---------------------------------------------------------------------------------------------
// Rust-backed natives. Each is a `RegularFn`. They read typed-array bytes zero-copy and return
// either an encoded string or a JS byte `Array`.
// ---------------------------------------------------------------------------------------------

fn type_error<'a>(agent: &mut Agent, msg: &'static str, gc: NoGcScope<'a, '_>) -> JsError<'a> {
    agent.throw_exception_with_static_message(ExceptionType::TypeError, msg, gc)
}

fn js_str<'gc>(agent: &mut Agent, s: &str, gc: NoGcScope<'gc, '_>) -> Value<'gc> {
    JsString::from_string(agent, s.to_owned(), gc).into()
}

/// Inspect a typed-array argument and return its backing `(buffer, byte_offset, len)`.
///
/// The returned [`ArrayBuffer`] handle is `unbind`ed (it is `Copy` and carries no borrow of
/// `agent`). `None` when the value is not a typed array or its backing buffer is detached.
fn ta_view(agent: &Agent, value: Value) -> Option<(ArrayBuffer<'static>, usize, usize)> {
    let ta = TypedArray::try_from(value).ok()?;
    let buf = ta.get_viewed_array_buffer(agent);
    if buf.is_detached(agent) {
        return None;
    }
    let offset = ta.byte_offset(agent);
    let len = ta
        .array_length(agent)
        .unwrap_or_else(|| buf.byte_length(agent).saturating_sub(offset));
    Some((buf.unbind(), offset, len))
}

/// Read the bytes of typed-array argument `index` as an owned `Vec<u8>`.
///
/// Owned (not borrowed) so the digest/HMAC/cipher computation does not hold a borrow of `agent` while
/// it later re-borrows `agent` mutably to build the result string/array. Hash/cipher inputs are the
/// only copy in this layer and are bounded by the caller's data.
fn arg_bytes(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<Vec<u8>> {
    let (buf, offset, len) = ta_view(agent, args.get(index))?;
    let slice = buf.as_slice(agent);
    slice
        .get(offset..offset.saturating_add(len))
        .map(<[u8]>::to_vec)
}

/// Read a JS string argument into an owned `String`.
fn arg_string(agent: &Agent, args: &ArgumentsList, index: usize) -> Option<String> {
    let s = JsString::try_from(args.get(index)).ok()?;
    s.as_str(agent).map(str::to_owned)
}

/// `digestEncoded(alg: string, data: Uint8Array, encoding: string) -> string`
fn native_digest_encoded<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(alg_name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "digest expects an algorithm name", nogc));
    };
    let Some(alg) = Algorithm::from_name(&alg_name) else {
        return Err(type_error(agent, "Digest method not supported", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "digest expects a Uint8Array of input", nogc));
    };
    let encoding = arg_string(agent, &args, 2).unwrap_or_else(|| "hex".to_owned());
    let out = encode_digest(&digest(alg, &data), &encoding);
    Ok(js_str(agent, &out, nogc))
}

/// `digestBytes(alg: string, data: Uint8Array) -> number[]` — the raw digest as a JS byte array, for
/// `digest()` with no encoding (the prelude wraps it in a `Uint8Array`/`Buffer`).
fn native_digest_bytes<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(alg_name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "digest expects an algorithm name", nogc));
    };
    let Some(alg) = Algorithm::from_name(&alg_name) else {
        return Err(type_error(agent, "Digest method not supported", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "digest expects a Uint8Array of input", nogc));
    };
    let bytes = digest(alg, &data);
    Ok(bytes_to_array(agent, &bytes, nogc).into())
}

/// `hmacEncoded(alg: string, key: Uint8Array, data: Uint8Array, encoding: string) -> string`
fn native_hmac_encoded<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(alg_name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "hmac expects an algorithm name", nogc));
    };
    let Some(alg) = Algorithm::from_name(&alg_name) else {
        return Err(type_error(agent, "Digest method not supported", nogc));
    };
    let Some(key) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "hmac expects a Uint8Array key", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 2) else {
        return Err(type_error(agent, "hmac expects a Uint8Array of input", nogc));
    };
    let encoding = arg_string(agent, &args, 3).unwrap_or_else(|| "hex".to_owned());
    let out = encode_digest(&hmac(alg, &key, &data), &encoding);
    Ok(js_str(agent, &out, nogc))
}

/// `hmacBytes(alg: string, key: Uint8Array, data: Uint8Array) -> number[]`
fn native_hmac_bytes<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(alg_name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "hmac expects an algorithm name", nogc));
    };
    let Some(alg) = Algorithm::from_name(&alg_name) else {
        return Err(type_error(agent, "Digest method not supported", nogc));
    };
    let Some(key) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "hmac expects a Uint8Array key", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 2) else {
        return Err(type_error(agent, "hmac expects a Uint8Array of input", nogc));
    };
    let bytes = hmac(alg, &key, &data);
    Ok(bytes_to_array(agent, &bytes, nogc).into())
}

/// `randomFill(out: Uint8Array) -> undefined` — fill the whole view with CSPRNG bytes in place.
fn native_random_fill<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some((buf, offset, len)) = ta_view(agent, args.get(0)) else {
        return Err(type_error(agent, "randomFill expects a Uint8Array", nogc));
    };
    // Generate into a scratch buffer (the CSPRNG borrow must not overlap the agent borrow), then
    // copy into the view. `len` is bounded by the caller's buffer.
    let mut scratch = vec![0u8; len];
    if random_fill(&mut scratch).is_err() {
        return Err(agent.throw_exception_with_static_message(
            ExceptionType::Error,
            "the OS CSPRNG failed",
            nogc,
        ));
    }
    let dst = buf.as_mut_slice(agent);
    if let Some(region) = dst.get_mut(offset..offset + len) {
        region.copy_from_slice(&scratch);
    }
    Ok(Value::Undefined)
}

/// `randomUuid() -> string` — an RFC 4122 v4 UUID.
fn native_random_uuid<'gc>(
    agent: &mut Agent,
    _this: Value,
    _args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    match random_uuid() {
        Ok(s) => Ok(js_str(agent, &s, nogc)),
        Err(_) => Err(agent.throw_exception_with_static_message(
            ExceptionType::Error,
            "the OS CSPRNG failed",
            nogc,
        )),
    }
}

/// `supports(alg: string) -> boolean` — whether `alg` names a supported digest. Backs
/// `crypto.getHashes()` membership checks in the prelude.
fn native_supports<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let _ = gc;
    let ok = arg_string(agent, &args, 0)
        .as_deref()
        .and_then(Algorithm::from_name)
        .is_some();
    Ok(Value::Boolean(ok))
}

/// `cipher(name: string, key: Uint8Array, iv: Uint8Array, data: Uint8Array) -> number[]` — one-shot
/// AES-CBC encrypt. The prelude's `createCipheriv(...).update()/.final()` buffers chunks and calls
/// this once over the concatenation (a block cipher's output is a pure function of the whole input).
fn native_cipher<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "cipher expects an algorithm name", nogc));
    };
    let Some(c) = Cipher::from_name(&name) else {
        return Err(type_error(agent, "Unsupported cipher", nogc));
    };
    let Some(key) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "cipher expects a Uint8Array key", nogc));
    };
    let Some(iv) = arg_bytes(agent, &args, 2) else {
        return Err(type_error(agent, "cipher expects a Uint8Array iv", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 3) else {
        return Err(type_error(agent, "cipher expects a Uint8Array of input", nogc));
    };
    match cipher_encrypt(c, &key, &iv, &data) {
        Ok(out) => Ok(bytes_to_array(agent, &out, nogc).into()),
        Err(CipherError::BadKeyOrIv) => Err(agent.throw_exception(
            ExceptionType::RangeError,
            "Invalid key length or initialization vector length".to_owned(),
            nogc,
        )),
        Err(CipherError::BadData) => Err(agent.throw_exception(
            ExceptionType::Error,
            "error while encrypting".to_owned(),
            nogc,
        )),
    }
}

/// `decipher(name: string, key: Uint8Array, iv: Uint8Array, data: Uint8Array) -> number[]` — one-shot
/// AES-CBC decrypt with PKCS#7 unpadding.
fn native_decipher<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "decipher expects an algorithm name", nogc));
    };
    let Some(c) = Cipher::from_name(&name) else {
        return Err(type_error(agent, "Unsupported cipher", nogc));
    };
    let Some(key) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "decipher expects a Uint8Array key", nogc));
    };
    let Some(iv) = arg_bytes(agent, &args, 2) else {
        return Err(type_error(agent, "decipher expects a Uint8Array iv", nogc));
    };
    let Some(data) = arg_bytes(agent, &args, 3) else {
        return Err(type_error(agent, "decipher expects a Uint8Array of input", nogc));
    };
    match cipher_decrypt(c, &key, &iv, &data) {
        Ok(out) => Ok(bytes_to_array(agent, &out, nogc).into()),
        Err(CipherError::BadKeyOrIv) => Err(agent.throw_exception(
            ExceptionType::RangeError,
            "Invalid key length or initialization vector length".to_owned(),
            nogc,
        )),
        Err(CipherError::BadData) => Err(agent.throw_exception(
            ExceptionType::Error,
            "error:1C800064:Provider routines::bad decrypt".to_owned(),
            nogc,
        )),
    }
}

/// `pbkdf2(alg: string, password: Uint8Array, salt: Uint8Array, iterations: number, keylen: number)
/// -> number[]` — derive a key. Backs both `pbkdf2Sync` and the async `pbkdf2` (the prelude delivers
/// the async result on a microtask).
fn native_pbkdf2<'gc>(
    agent: &mut Agent,
    _this: Value,
    args: ArgumentsList,
    gc: GcScope<'gc, '_>,
) -> JsResult<'gc, Value<'gc>> {
    let nogc = gc.into_nogc();
    let Some(alg_name) = arg_string(agent, &args, 0) else {
        return Err(type_error(agent, "pbkdf2 expects a digest name", nogc));
    };
    let Some(alg) = Algorithm::from_name(&alg_name) else {
        return Err(type_error(agent, "Digest method not supported", nogc));
    };
    let Some(password) = arg_bytes(agent, &args, 1) else {
        return Err(type_error(agent, "pbkdf2 expects a Uint8Array password", nogc));
    };
    let Some(salt) = arg_bytes(agent, &args, 2) else {
        return Err(type_error(agent, "pbkdf2 expects a Uint8Array salt", nogc));
    };
    let iterations = match args.get(3) {
        Value::Integer(i) => i.into_i64().max(1) as u32,
        Value::SmallF64(f) => (f.into_f64() as i64).max(1) as u32,
        _ => return Err(type_error(agent, "pbkdf2 expects an iterations count", nogc)),
    };
    let keylen = match args.get(4) {
        Value::Integer(i) => i.into_i64().max(0) as usize,
        Value::SmallF64(f) => (f.into_f64() as i64).max(0) as usize,
        _ => return Err(type_error(agent, "pbkdf2 expects a key length", nogc)),
    };
    let derived = pbkdf2(alg, &password, &salt, iterations, keylen);
    Ok(bytes_to_array(agent, &derived, nogc).into())
}

/// Build a JS `Array` whose elements are the given bytes (each a small integer `0..=255`).
///
/// The pinned Nova rev exposes no embedder-side slice-to-`Uint8Array` constructor (every typed-array
/// allocator is `pub(crate)`), so the bytes cross into JS as a plain byte `Array`, exactly as
/// `node:text_encoding` does; the prelude wraps it with the JS `Uint8Array` constructor. Each element
/// is a tagged small integer, so there is no per-byte heap allocation.
fn bytes_to_array<'gc>(agent: &mut Agent, bytes: &[u8], gc: NoGcScope<'gc, '_>) -> Array<'gc> {
    let values: Vec<Value> = bytes.iter().map(|&b| Value::from(b)).collect();
    Array::from_slice(agent, &values, gc)
}

/// The JS prelude. Built once per runtime; a `&'static str` so an un-imported runtime pays nothing.
///
/// It is an IIFE that reads the Rust natives off the private global key, defines `Hash`, `Hmac`,
/// `Cipheriv`, `Decipheriv`, `createHash`, `createHmac`, `createCipheriv`, `createDecipheriv`,
/// `pbkdf2`/`pbkdf2Sync`, `randomBytes`, `randomFillSync`, `randomUUID`, `randomInt`,
/// `timingSafeEqual`, and `getHashes`, and returns the module exports object. Kept inline (rather
/// than a sibling file) so the whole module is one self-contained unit.
const PRELUDE: &str = r##"
(function () {
  const N = globalThis["__treaty_crypto_natives__"];

  // Coerce a `data`/`key` argument to a Uint8Array of its bytes. Accepts strings (utf8 / hex /
  // base64 / latin1 per the optional encoding), and any ArrayBuffer view / ArrayBuffer.
  function toBytes(value, encoding) {
    if (value instanceof Uint8Array) return value;
    if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
    if (value instanceof ArrayBuffer) return new Uint8Array(value);
    if (typeof value === "string") return strToBytes(value, encoding);
    throw new TypeError("expected a string, Buffer, TypedArray, or ArrayBuffer");
  }

  function strToBytes(s, encoding) {
    const e = (encoding === undefined || encoding === null) ? "utf8" : String(encoding).toLowerCase();
    switch (e) {
      case "utf8": case "utf-8": {
        // UTF-8 encode without depending on TextEncoder being present.
        const out = [];
        for (let i = 0; i < s.length; i++) {
          let c = s.charCodeAt(i);
          if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
            const c2 = s.charCodeAt(i + 1);
            if (c2 >= 0xdc00 && c2 <= 0xdfff) { c = 0x10000 + ((c - 0xd800) << 10) + (c2 - 0xdc00); i++; }
          }
          if (c < 0x80) out.push(c);
          else if (c < 0x800) { out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f)); }
          else if (c < 0x10000) { out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f)); }
          else { out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f)); }
        }
        return Uint8Array.from(out);
      }
      case "hex": {
        const n = s.length >> 1;
        const out = new Uint8Array(n);
        for (let i = 0; i < n; i++) out[i] = parseInt(s.substr(i * 2, 2), 16) & 0xff;
        return out;
      }
      case "latin1": case "binary": case "ascii": {
        const out = new Uint8Array(s.length);
        for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i) & 0xff;
        return out;
      }
      case "base64": case "base64url": {
        const alpha = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789" + (e === "base64url" ? "-_" : "+/");
        const lut = {};
        for (let i = 0; i < alpha.length; i++) lut[alpha[i]] = i;
        let acc = 0, bits = 0; const out = [];
        for (let i = 0; i < s.length; i++) {
          const ch = s[i];
          if (ch === "=" ) break;
          const v = lut[ch];
          if (v === undefined) continue;
          acc = (acc << 6) | v; bits += 6;
          if (bits >= 8) { bits -= 8; out.push((acc >> bits) & 0xff); }
        }
        return Uint8Array.from(out);
      }
      default: throw new TypeError("Unknown encoding: " + encoding);
    }
  }

  // Encode raw bytes to a string per a Node output encoding (the inverse of strToBytes).
  function bytesToString(u8, encoding) {
    const e = String(encoding).toLowerCase();
    switch (e) {
      case "hex": {
        let s = ""; const H = "0123456789abcdef";
        for (let i = 0; i < u8.length; i++) s += H[u8[i] >> 4] + H[u8[i] & 0xf];
        return s;
      }
      case "latin1": case "binary": case "ascii": {
        let s = ""; for (let i = 0; i < u8.length; i++) s += String.fromCharCode(u8[i]); return s;
      }
      case "base64": case "base64url": {
        const alpha = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789" + (e === "base64url" ? "-_" : "+/");
        let s = "";
        for (let i = 0; i < u8.length; i += 3) {
          const b0 = u8[i], b1 = i + 1 < u8.length ? u8[i + 1] : 0, b2 = i + 2 < u8.length ? u8[i + 2] : 0;
          const n = (b0 << 16) | (b1 << 8) | b2;
          s += alpha[(n >> 18) & 63] + alpha[(n >> 12) & 63];
          s += (i + 1 < u8.length) ? alpha[(n >> 6) & 63] : (e === "base64url" ? "" : "=");
          s += (i + 2 < u8.length) ? alpha[n & 63] : (e === "base64url" ? "" : "=");
        }
        return s;
      }
      case "utf8": case "utf-8": {
        let s = "", i = 0;
        while (i < u8.length) {
          const c = u8[i];
          if (c < 0x80) { s += String.fromCharCode(c); i += 1; }
          else if (c < 0xe0) { s += String.fromCharCode(((c & 0x1f) << 6) | (u8[i + 1] & 0x3f)); i += 2; }
          else if (c < 0xf0) { s += String.fromCharCode(((c & 0x0f) << 12) | ((u8[i + 1] & 0x3f) << 6) | (u8[i + 2] & 0x3f)); i += 3; }
          else {
            const cp = ((c & 0x07) << 18) | ((u8[i + 1] & 0x3f) << 12) | ((u8[i + 2] & 0x3f) << 6) | (u8[i + 3] & 0x3f);
            const v = cp - 0x10000;
            s += String.fromCharCode(0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff)); i += 4;
          }
        }
        return s;
      }
      default: throw new TypeError("Unknown encoding: " + encoding);
    }
  }

  // Wrap a byte array (or byte source) as a Buffer if node:buffer has been loaded; otherwise return
  // a plain Uint8Array (a faithful byte container either way).
  function asBuffer(byteArray) {
    const u8 = byteArray instanceof Uint8Array ? byteArray : Uint8Array.from(byteArray);
    try {
      const buf = (typeof require === "function") ? require("node:buffer") : null;
      if (buf && buf.Buffer) return buf.Buffer.from(u8);
    } catch (_) { /* buffer not available; fall through */ }
    return u8;
  }

  function concatBytes(chunks) {
    let total = 0;
    for (const c of chunks) total += c.length;
    const all = new Uint8Array(total);
    let pos = 0;
    for (const c of chunks) { all.set(c, pos); pos += c.length; }
    return all;
  }

  class Hash {
    constructor(algorithm) {
      if (!N.supports(algorithm)) throw new Error("Digest method not supported");
      this._alg = algorithm;
      this._chunks = [];
      this._finalized = false;
    }
    update(data, inputEncoding) {
      if (this._finalized) throw new Error("Digest already called");
      this._chunks.push(toBytes(data, inputEncoding));
      return this;
    }
    digest(encoding) {
      if (this._finalized) throw new Error("Digest already called");
      this._finalized = true;
      const all = concatBytes(this._chunks);
      if (encoding === undefined || encoding === null) return asBuffer(N.digestBytes(this._alg, all));
      return N.digestEncoded(this._alg, all, String(encoding).toLowerCase());
    }
  }

  class Hmac {
    constructor(algorithm, key, options) {
      if (!N.supports(algorithm)) throw new Error("Digest method not supported");
      this._alg = algorithm;
      this._key = toBytes(key, options && options.encoding);
      this._chunks = [];
      this._finalized = false;
    }
    update(data, inputEncoding) {
      if (this._finalized) throw new Error("Digest already called");
      this._chunks.push(toBytes(data, inputEncoding));
      return this;
    }
    digest(encoding) {
      if (this._finalized) throw new Error("Digest already called");
      this._finalized = true;
      const all = concatBytes(this._chunks);
      if (encoding === undefined || encoding === null) return asBuffer(N.hmacBytes(this._alg, this._key, all));
      return N.hmacEncoded(this._alg, this._key, all, String(encoding).toLowerCase());
    }
  }

  function createHash(algorithm, options) { return new Hash(String(algorithm).toLowerCase()); }
  function createHmac(algorithm, key, options) { return new Hmac(String(algorithm).toLowerCase(), key, options); }

  // ----- symmetric ciphers (AES-CBC) -----------------------------------------------------------
  //
  // createCipheriv/createDecipheriv return an object that buffers update() chunks and runs the
  // single native cipher/decipher call on final(). A block cipher's CBC output is a pure function of
  // the whole padded input, so buffering-then-one-shot is byte-identical to a streaming cipher.

  class Cipheriv {
    constructor(algorithm, key, iv) {
      this._alg = String(algorithm).toLowerCase();
      this._key = toBytes(key);
      this._iv = toBytes(iv);
      this._chunks = [];
      this._final = false;
    }
    update(data, inputEncoding, outputEncoding) {
      if (this._final) throw new Error("Trying to add data in unsupported state");
      this._chunks.push(toBytes(data, inputEncoding));
      // Defer all output to final() so the single padded one-shot is byte-exact; update() yields an
      // empty buffer (Node permits update() to return 0 bytes until a block boundary).
      return asBuffer(new Uint8Array(0));
    }
    final(outputEncoding) {
      if (this._final) throw new Error("Trying to add data in unsupported state");
      this._final = true;
      const out = N.cipher(this._alg, this._key, this._iv, concatBytes(this._chunks));
      const u8 = Uint8Array.from(out);
      if (outputEncoding === undefined || outputEncoding === null) return asBuffer(u8);
      return bytesToString(u8, outputEncoding);
    }
  }

  class Decipheriv {
    constructor(algorithm, key, iv) {
      this._alg = String(algorithm).toLowerCase();
      this._key = toBytes(key);
      this._iv = toBytes(iv);
      this._chunks = [];
      this._final = false;
    }
    update(data, inputEncoding, outputEncoding) {
      if (this._final) throw new Error("Trying to add data in unsupported state");
      this._chunks.push(toBytes(data, inputEncoding));
      return asBuffer(new Uint8Array(0));
    }
    final(outputEncoding) {
      if (this._final) throw new Error("Trying to add data in unsupported state");
      this._final = true;
      const out = N.decipher(this._alg, this._key, this._iv, concatBytes(this._chunks));
      const u8 = Uint8Array.from(out);
      if (outputEncoding === undefined || outputEncoding === null) return asBuffer(u8);
      return bytesToString(u8, outputEncoding);
    }
  }

  function createCipheriv(algorithm, key, iv, options) { return new Cipheriv(algorithm, key, iv); }
  function createDecipheriv(algorithm, key, iv, options) { return new Decipheriv(algorithm, key, iv); }

  // ----- PBKDF2 --------------------------------------------------------------------------------

  function pbkdf2Sync(password, salt, iterations, keylen, digest) {
    const p = toBytes(password), s = toBytes(salt);
    const d = String(digest || "sha1").toLowerCase();
    return asBuffer(N.pbkdf2(d, p, s, iterations >>> 0, keylen >>> 0));
  }

  function pbkdf2(password, salt, iterations, keylen, digest, callback) {
    // The 5-arg form (no explicit digest) passes the callback as `digest`.
    let dig = digest, cb = callback;
    if (typeof digest === "function") { cb = digest; dig = undefined; }
    if (typeof cb !== "function") throw new TypeError("callback must be a function");
    let result, err = null;
    try { result = pbkdf2Sync(password, salt, iterations, keylen, dig); }
    catch (e) { err = e; }
    queueMicrotask(() => err ? cb(err) : cb(null, result));
    return undefined;
  }

  // scryptSync / scrypt: no pure-Rust scrypt crate is vendored offline, so these are a documented
  // follow-up. They throw a clear, catchable Error rather than silently returning wrong bytes.
  function scryptSync() { throw new Error("crypto.scryptSync is not implemented in this runtime"); }
  function scrypt() { throw new Error("crypto.scrypt is not implemented in this runtime"); }

  // randomBytes(size[, cb]) -> Buffer (or async via callback).
  function randomBytes(size, callback) {
    const n = size >>> 0;
    const u8 = new Uint8Array(n);
    if (typeof callback === "function") {
      // Async form: still synchronous fill, delivered on a microtask (no real I/O here).
      try { N.randomFill(u8); } catch (err) { queueMicrotask(() => callback(err)); return undefined; }
      const out = asBuffer(u8);
      queueMicrotask(() => callback(null, out));
      return undefined;
    }
    N.randomFill(u8);
    return asBuffer(u8);
  }

  // randomFillSync(buf[, offset[, size]]) -> buf
  function randomFillSync(buf, offset, size) {
    if (!(buf instanceof Uint8Array) && !ArrayBuffer.isView(buf)) throw new TypeError("randomFillSync expects a TypedArray");
    const view = buf instanceof Uint8Array ? buf : new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
    const off = offset === undefined ? 0 : offset >>> 0;
    const len = size === undefined ? view.length - off : Math.min(size >>> 0, view.length - off);
    const region = view.subarray(off, off + len);
    N.randomFill(region);
    return buf;
  }

  function randomUUID(options) { return N.randomUuid(); }

  // randomInt([min, ]max[, cb]) -> a uniform integer in [min, max) using rejection sampling.
  function randomInt(a, b, c) {
    let min, max, cb;
    if (typeof b === "function") { cb = b; min = 0; max = a; }
    else if (b === undefined) { min = 0; max = a; }
    else { min = a; max = b; cb = c; }
    min = Math.floor(min); max = Math.floor(max);
    if (!(max > min)) throw new RangeError("The value of \"max\" is out of range");
    const range = max - min;
    const compute = () => {
      // Draw 6 bytes (48 bits) and reject the top non-uniform tail.
      const u8 = new Uint8Array(6);
      const limit = Math.floor(0x1000000000000 / range) * range;
      let x;
      do {
        N.randomFill(u8);
        x = 0;
        for (let i = 0; i < 6; i++) x = x * 256 + u8[i];
      } while (x >= limit);
      return min + (x % range);
    };
    if (typeof cb === "function") {
      let v, err = null;
      try { v = compute(); } catch (e) { err = e; }
      queueMicrotask(() => err ? cb(err) : cb(null, v));
      return undefined;
    }
    return compute();
  }

  // timingSafeEqual(a, b) -> boolean, constant-time over equal-length inputs (throws on length
  // mismatch, matching Node).
  function timingSafeEqual(a, b) {
    const ba = toBytes(a), bb = toBytes(b);
    if (ba.length !== bb.length) throw new RangeError("Input buffers must have the same byte length");
    let diff = 0;
    for (let i = 0; i < ba.length; i++) diff |= ba[i] ^ bb[i];
    return diff === 0;
  }

  const HASHES = ["md5", "sha1", "sha224", "sha256", "sha384", "sha512"];
  function getHashes() { return HASHES.slice(); }

  return {
    createHash,
    createHmac,
    createCipheriv,
    createDecipheriv,
    pbkdf2,
    pbkdf2Sync,
    scrypt,
    scryptSync,
    randomBytes,
    randomFillSync,
    randomUUID,
    randomInt,
    timingSafeEqual,
    getHashes,
    Hash,
    Hmac,
  };
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::core::{EnvMap, HostState, NodeCtx};
    use nova_vm::ecmascript::{
        AgentOptions, GcAgent, String as JsString, parse_script, script_evaluation,
    };
    use serde_json::{json, Value as JsonValue};

    // ----- pure-core unit tests (no JS agent) -------------------------------------------------

    #[test]
    fn sha256_of_abc_matches_known_hex() {
        // The canonical FIPS 180-2 test vector for "abc".
        let d = digest(Algorithm::Sha256, b"abc");
        assert_eq!(
            to_hex(&d),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_of_empty_matches_known_hex() {
        let d = digest(Algorithm::Sha256, b"");
        assert_eq!(
            to_hex(&d),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha1_of_abc_matches_known_hex() {
        let d = digest(Algorithm::Sha1, b"abc");
        assert_eq!(to_hex(&d), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }

    #[test]
    fn md5_of_abc_matches_known_hex() {
        let d = digest(Algorithm::Md5, b"abc");
        assert_eq!(to_hex(&d), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn sha512_and_sha384_and_sha224_known_hex() {
        assert_eq!(
            to_hex(&digest(Algorithm::Sha224, b"abc")),
            "23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7"
        );
        assert_eq!(
            to_hex(&digest(Algorithm::Sha384, b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            to_hex(&digest(Algorithm::Sha512, b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn hmac_sha256_rfc4231_test_case_2() {
        // RFC 4231 test case 2: key "Jefe", data "what do ya want for nothing?".
        let mac = hmac(Algorithm::Sha256, b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            to_hex(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_sha1_rfc2202_test_case_2() {
        // RFC 2202 HMAC-SHA1 test case 2.
        let mac = hmac(Algorithm::Sha1, b"Jefe", b"what do ya want for nothing?");
        assert_eq!(to_hex(&mac), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn algorithm_name_parsing_is_lenient() {
        assert_eq!(Algorithm::from_name("SHA256"), Some(Algorithm::Sha256));
        assert_eq!(Algorithm::from_name("sha-256"), Some(Algorithm::Sha256));
        assert_eq!(Algorithm::from_name("  Sha1 "), Some(Algorithm::Sha1));
        assert_eq!(Algorithm::from_name("MD5"), Some(Algorithm::Md5));
        assert_eq!(Algorithm::from_name("sha3-256"), None);
        assert_eq!(Algorithm::from_name("ripemd160"), None);
    }

    #[test]
    fn output_len_matches_digest_len() {
        for alg in [
            Algorithm::Md5,
            Algorithm::Sha1,
            Algorithm::Sha224,
            Algorithm::Sha256,
            Algorithm::Sha384,
            Algorithm::Sha512,
        ] {
            assert_eq!(digest(alg, b"x").len(), alg.output_len());
        }
    }

    #[test]
    fn random_bytes_returns_requested_length() {
        for n in [0usize, 1, 16, 32, 1000] {
            assert_eq!(random_bytes(n).unwrap().len(), n);
        }
    }

    #[test]
    fn random_bytes_are_not_all_zero_for_large_n() {
        // Astronomically unlikely to be all zeros; guards against a no-op CSPRNG.
        let b = random_bytes(64).unwrap();
        assert!(b.iter().any(|&x| x != 0));
    }

    #[test]
    fn random_uuid_has_v4_shape() {
        let u = random_uuid().unwrap();
        assert_eq!(u.len(), 36);
        let bytes = u.as_bytes();
        // Hyphens at the canonical 8-4-4-4-12 positions.
        for &i in &[8usize, 13, 18, 23] {
            assert_eq!(bytes[i], b'-', "expected hyphen at {i} in {u}");
        }
        // Version nibble is '4'; variant nibble is one of 8/9/a/b.
        assert_eq!(bytes[14], b'4', "version digit must be 4 in {u}");
        assert!(matches!(bytes[19], b'8' | b'9' | b'a' | b'b'), "variant digit in {u}");
        // All non-hyphen characters are lowercase hex.
        for (i, &c) in bytes.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                continue;
            }
            assert!(c.is_ascii_hexdigit() && !c.is_ascii_uppercase(), "non-lowercase-hex {c} in {u}");
        }
    }

    #[test]
    fn random_uuids_are_distinct() {
        let a = random_uuid().unwrap();
        let b = random_uuid().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn base64_and_base64url_encoders_match_known_vectors() {
        // "Man" -> "TWFu"; one byte 0xff -> "/w==" std, "_w" url (no padding).
        assert_eq!(to_base64(b"Man"), "TWFu");
        assert_eq!(to_base64(&[0xff]), "/w==");
        assert_eq!(to_base64url(&[0xff]), "_w");
        assert_eq!(to_base64(b""), "");
    }

    #[test]
    fn aes_256_cbc_round_trips_with_known_key_iv() {
        let key = [0x42u8; 32];
        let iv = [0x24u8; 16];
        let plaintext = b"attack at dawn -- but only if it is sunny";
        let ct = cipher_encrypt(Cipher::Aes256Cbc, &key, &iv, plaintext).unwrap();
        // PKCS#7-padded to a whole number of 16-byte blocks, strictly larger than the input.
        assert_eq!(ct.len() % 16, 0);
        assert!(ct.len() >= plaintext.len());
        assert_ne!(&ct[..], &plaintext[..]);
        let pt = cipher_decrypt(Cipher::Aes256Cbc, &key, &iv, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aes_256_cbc_matches_known_answer_vector() {
        // NIST SP 800-38A F.2.5 CBC-AES256.Encrypt, first block.
        let key = hex_to_bytes("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let iv = hex_to_bytes("000102030405060708090a0b0c0d0e0f");
        let pt = hex_to_bytes("6bc1bee22e409f96e93d7e117393172a");
        let ct = cipher_encrypt(Cipher::Aes256Cbc, &key, &iv, &pt).unwrap();
        // SP 800-38A expected first ciphertext block (the PKCS#7 pad adds a second block we ignore).
        assert_eq!(to_hex(&ct[..16]), "f58c4c04d6e5f1ba779eabfb5f7bfbd6");
    }

    #[test]
    fn aes_128_cbc_round_trips() {
        let key = [0x01u8; 16];
        let iv = [0x02u8; 16];
        let pt = b"sixteen-byte-block boundaries are exercised too";
        let ct = cipher_encrypt(Cipher::Aes128Cbc, &key, &iv, pt).unwrap();
        assert_eq!(cipher_decrypt(Cipher::Aes128Cbc, &key, &iv, &ct).unwrap(), pt);
    }

    #[test]
    fn cipher_rejects_wrong_key_or_iv_length() {
        assert_eq!(
            cipher_encrypt(Cipher::Aes256Cbc, &[0u8; 16], &[0u8; 16], b"x"),
            Err(CipherError::BadKeyOrIv)
        );
        assert_eq!(
            cipher_encrypt(Cipher::Aes256Cbc, &[0u8; 32], &[0u8; 8], b"x"),
            Err(CipherError::BadKeyOrIv)
        );
    }

    #[test]
    fn decipher_rejects_corrupt_ciphertext() {
        let key = [0x42u8; 32];
        let iv = [0x24u8; 16];
        // 16 zero bytes almost never decrypt to valid PKCS#7 padding.
        assert_eq!(
            cipher_decrypt(Cipher::Aes256Cbc, &key, &iv, &[0u8; 16]),
            Err(CipherError::BadData)
        );
        // A non-block-multiple length is always rejected.
        assert_eq!(
            cipher_decrypt(Cipher::Aes256Cbc, &key, &iv, &[0u8; 5]),
            Err(CipherError::BadData)
        );
    }

    #[test]
    fn cipher_name_parsing() {
        assert_eq!(Cipher::from_name("aes-256-cbc"), Some(Cipher::Aes256Cbc));
        assert_eq!(Cipher::from_name("AES-128-CBC"), Some(Cipher::Aes128Cbc));
        assert_eq!(Cipher::from_name("aes-192-cbc"), None);
        assert_eq!(Cipher::from_name("chacha20"), None);
    }

    #[test]
    fn pbkdf2_sha256_known_answer() {
        // RFC 7914 §11 PBKDF2-HMAC-SHA-256: P="passwd", S="salt", c=1, dkLen=64.
        let dk = pbkdf2(Algorithm::Sha256, b"passwd", b"salt", 1, 64);
        assert_eq!(
            to_hex(&dk),
            "55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc\
49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783"
        );
    }

    #[test]
    fn pbkdf2_sha256_second_known_answer() {
        // RFC 7914 §11 second vector: P="Password", S="NaCl", c=80000, dkLen=64.
        let dk = pbkdf2(Algorithm::Sha256, b"Password", b"NaCl", 80000, 64);
        assert_eq!(
            to_hex(&dk),
            "4ddcd8f60b98be21830cee5ef22701f9641a4418d04c0414aeff08876b34ab56\
a1d425a1225833549adb841b51c9b3176a272bdebba1d078478f62b397f33c8d"
        );
    }

    #[test]
    fn pbkdf2_sha1_known_answer() {
        // RFC 6070 PBKDF2-HMAC-SHA1: P="password", S="salt", c=1, dkLen=20.
        let dk = pbkdf2(Algorithm::Sha1, b"password", b"salt", 1, 20);
        assert_eq!(to_hex(&dk), "0c60c80f961f0e71f3a9b524af6012062fe037a6");
    }

    /// Decode a hex string into bytes (test helper).
    fn hex_to_bytes(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // ----- JS-surface integration tests (live engine) -----------------------------------------

    /// Install the `node:crypto` module directly and evaluate `src` against it, returning the JSON
    /// of `({ v: (<src>) })`. Mirrors the buffer module's self-contained harness.
    fn run(src: &str) -> JsonValue {
        let host_state: &'static HostState = Box::leak(Box::new(HostState::new(
            std::env::current_dir().unwrap(),
            EnvMap::new(),
        )));

        let mut agent = GcAgent::new(
            AgentOptions {
                disable_gc: false,
                print_internals: false,
                no_block: false,
            },
            host_state,
        );

        let create_global_object: Option<for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>> =
            None;
        let create_global_this_value: Option<
            for<'a> fn(&mut Agent, GcScope<'a, '_>) -> Object<'a>,
        > = None;
        let initialize_global: Option<fn(&mut Agent, Object, GcScope)> =
            Some(crate::node::install);
        let realm = agent.create_realm(
            create_global_object,
            create_global_this_value,
            initialize_global,
        );

        let out = agent.run_in_realm(&realm, |agent, mut gc| -> String {
            let ctx = NodeCtx::new(host_state);
            let exports = install(agent, &ctx, gc.reborrow())
                .expect("crypto install should succeed")
                .unbind();

            {
                let nogc = gc.nogc();
                let global = agent.current_realm(nogc).global_object(agent);
                let key = PropertyKey::from_static_str(agent, "C", nogc);
                let _ = global.unbind().try_define_own_property(
                    agent,
                    key.unbind(),
                    PropertyDescriptor::new_data_descriptor(exports.bind(nogc)),
                    None,
                    nogc,
                );
            }

            let wrapped = format!("JSON.stringify({{ v: ({src}) }})");
            let source = JsString::from_string(agent, wrapped, gc.nogc());
            let current = agent.current_realm(gc.nogc());
            let script = parse_script(agent, source.unbind(), current.unbind(), true, None, gc.nogc())
                .expect("test script parses");
            let value = script_evaluation(agent, script.unbind(), gc.reborrow())
                .unbind()
                .bind(gc.nogc());
            match value {
                Ok(v) => v
                    .unbind()
                    .string_repr(agent, gc.reborrow())
                    .to_string_lossy(agent)
                    .into_owned(),
                Err(e) => panic!(
                    "test script threw: {}",
                    e.value()
                        .unbind()
                        .string_repr(agent, gc.reborrow())
                        .to_string_lossy(agent)
                ),
            }
        });

        let envelope: JsonValue = serde_json::from_str(&out).expect("result is JSON");
        match envelope {
            JsonValue::Object(mut m) => m.remove("v").unwrap_or(JsonValue::Null),
            other => other,
        }
    }

    #[test]
    fn create_hash_sha256_hex_matches_node() {
        let v = run("C.createHash('sha256').update('abc').digest('hex')");
        assert_eq!(
            v,
            json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn create_hash_streaming_updates_concatenate() {
        let v = run(
            "C.createHash('sha256').update('a').update('b').update('c').digest('hex')",
        );
        assert_eq!(
            v,
            json!("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
    }

    #[test]
    fn create_hash_base64_and_no_encoding_bytes() {
        let v = run(
            "(() => {
               const b64 = C.createHash('sha1').update('abc').digest('base64');
               const raw = C.createHash('sha1').update('abc').digest();
               return [b64, raw.length, raw[0]];
             })()",
        );
        // sha1('abc') = a9 99 3e 36 ... ; base64 of that 20-byte digest, and first raw byte 0xa9.
        assert_eq!(v, json!(["qZk+NkcGgWq6PiVxeFDCbJzQ2J0=", 20, 0xa9]));
    }

    #[test]
    fn create_hmac_sha256_hex_matches_rfc4231() {
        let v = run(
            "C.createHmac('sha256', 'Jefe').update('what do ya want for nothing?').digest('hex')",
        );
        assert_eq!(
            v,
            json!("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
    }

    #[test]
    fn random_bytes_length_and_type() {
        let v = run(
            "(() => { const b = C.randomBytes(24); return [b.length, b instanceof Uint8Array]; })()",
        );
        assert_eq!(v, json!([24, true]));
    }

    #[test]
    fn random_fill_sync_fills_in_place() {
        let v = run(
            "(() => { const a = new Uint8Array(16); const r = C.randomFillSync(a);
               let nonzero = false; for (const x of a) if (x !== 0) nonzero = true;
               return [r === a, a.length, nonzero]; })()",
        );
        // (Astronomically unlikely that 16 random bytes are all zero.)
        assert_eq!(v, json!([true, 16, true]));
    }

    #[test]
    fn random_uuid_shape_in_js() {
        let v = run(
            "(() => { const u = C.randomUUID();
               return [u.length, u[14], u.split('-').length,
                       /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(u)]; })()",
        );
        assert_eq!(v, json!([36, "4", 5, true]));
    }

    #[test]
    fn timing_safe_equal_compares_bytes() {
        let v = run(
            "(() => {
               const a = C.createHash('sha256').update('x').digest();
               const b = C.createHash('sha256').update('x').digest();
               const c = C.createHash('sha256').update('y').digest();
               return [C.timingSafeEqual(a, b), C.timingSafeEqual(a, c)]; })()",
        );
        assert_eq!(v, json!([true, false]));
    }

    #[test]
    fn random_int_is_in_range() {
        let v = run(
            "(() => { let ok = true; for (let i = 0; i < 50; i++) { const n = C.randomInt(10, 20); if (n < 10 || n >= 20) ok = false; } return ok; })()",
        );
        assert_eq!(v, json!(true));
    }

    #[test]
    fn get_hashes_lists_supported_algorithms() {
        let v = run("C.getHashes().includes('sha256') && C.getHashes().includes('md5')");
        assert_eq!(v, json!(true));
    }

    #[test]
    fn unsupported_algorithm_throws() {
        let v = run(
            "(() => { try { C.createHash('sha3-512'); return 'no-throw'; } catch (e) { return 'threw'; } })()",
        );
        assert_eq!(v, json!("threw"));
    }

    #[test]
    fn aes_256_cbc_encrypt_then_decrypt_round_trips_in_js() {
        let v = run(
            "(() => {
               const Buffer = require('node:buffer').Buffer;
               const key = new Uint8Array(32).fill(0x42);
               const iv = new Uint8Array(16).fill(0x24);
               const c = C.createCipheriv('aes-256-cbc', key, iv);
               c.update('secret message');
               const ctHex = c.final('hex');
               const d = C.createDecipheriv('aes-256-cbc', key, iv);
               d.update(Buffer.from(ctHex, 'hex'));
               const pt = d.final('utf8');
               return pt; })()",
        );
        assert_eq!(v, json!("secret message"));
    }

    #[test]
    fn aes_256_cbc_known_answer_in_js() {
        // Same NIST SP 800-38A vector as the Rust core test, driven through the JS surface.
        let v = run(
            "(() => {
               const Buffer = require('node:buffer').Buffer;
               const key = Buffer.from('603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4', 'hex');
               const iv = Buffer.from('000102030405060708090a0b0c0d0e0f', 'hex');
               const c = C.createCipheriv('aes-256-cbc', key, iv);
               c.update(Buffer.from('6bc1bee22e409f96e93d7e117393172a', 'hex'));
               const ct = c.final('hex');
               return ct.slice(0, 32); })()",
        );
        assert_eq!(v, json!("f58c4c04d6e5f1ba779eabfb5f7bfbd6"));
    }

    #[test]
    fn pbkdf2_sync_known_answer_in_js() {
        let v = run("C.pbkdf2Sync('passwd', 'salt', 1, 64, 'sha256').toString('hex')");
        assert_eq!(
            v,
            json!("55ac046e56e3089fec1691c22544b605f94185216dde0465e68b9d57c20dacbc49ca9cccf179b645991664b39d77ef317c71b845b1e30bd509112041d3a19783")
        );
    }

    #[test]
    fn pbkdf2_sync_default_digest_is_sha1() {
        // Node defaults the digest to sha1 when omitted (with a deprecation warning, which we skip).
        let v = run("C.pbkdf2Sync('password', 'salt', 1, 20).toString('hex')");
        assert_eq!(v, json!("0c60c80f961f0e71f3a9b524af6012062fe037a6"));
    }

    #[test]
    fn create_cipheriv_rejects_wrong_key_length() {
        let v = run(
            "(() => { try {
                 const c = C.createCipheriv('aes-256-cbc', new Uint8Array(16), new Uint8Array(16));
                 c.update('x'); c.final();
                 return 'no-throw';
               } catch (e) { return 'threw'; } })()",
        );
        assert_eq!(v, json!("threw"));
    }

    #[test]
    fn scrypt_sync_throws_documented_unimplemented() {
        let v = run(
            "(() => { try { C.scryptSync('p', 's', 16); return 'no-throw'; } catch (e) { return 'threw'; } })()",
        );
        assert_eq!(v, json!("threw"));
    }
}
