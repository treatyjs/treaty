//! Message-id digest primitives — ported from Angular's `digest.ts`
//! (`computeMsgId` / `fingerprint` / `hash32` / `mix`).
//!
//! These are PURE byte-hash primitives (no dependency on `output_ast` or any
//! template/i18n type), so they live in `core` rather than the template layer.
//! The emitter (`core::output::emitter`) calls [`compute_msg_id`] directly; the
//! i18n module re-exports [`compute_msg_id`] / [`fingerprint`] from here for
//! byte-compat with its historical `crate::i18n::compute_msg_id` surface.

/// `digest.ts` `computeMsgId`: the XLIFF2/XMB/`$localize` message id.
///
/// Returns the 63-bit fingerprint as a decimal string. This is the value that
/// `output_ast`'s `LocalizedString` meta-block deferred to the compiler.
pub fn compute_msg_id(msg: &str, meaning: &str) -> String {
    let mut msg_fingerprint = fingerprint(msg.as_bytes());

    if !meaning.is_empty() {
        // Rotate the 64-bit message fingerprint one bit to the left, then add
        // the meaning fingerprint.
        msg_fingerprint = (msg_fingerprint << 1) | ((msg_fingerprint >> 63) & 1);
        msg_fingerprint = msg_fingerprint.wrapping_add(fingerprint(meaning.as_bytes()));
    }

    // BigInt.asUintN(63, ...) — keep the low 63 bits.
    let masked = msg_fingerprint & ((1u64 << 63) - 1);
    masked.to_string()
}

/// `digest.ts` `fingerprint`: 64-bit hash of a UTF-8 byte string.
///
/// based on closure-compiler's `GoogleJsMessageIdGenerator`.
pub fn fingerprint(utf8: &[u8]) -> u64 {
    let mut hi = hash32(utf8, 0);
    let mut lo = hash32(utf8, 102_072);

    if hi == 0 && (lo == 0 || lo == 1) {
        hi ^= 0x130f_9bef;
        // -0x6b5f56d8 as a 32-bit two's-complement value.
        lo ^= 0x94a0_a928;
    }

    ((hi as u64) << 32) | (lo as u64)
}

/// `digest.ts` `hash32`. Operates on `length - 12` chunks reading little-endian
/// 32-bit words, with a tail handling the remaining 0..=11 bytes.
///
/// All arithmetic is 32-bit wrapping (`number` ops are coerced to u32 via the
/// way `mix`/`>>>`/`<<` behave in JS; we replicate with `u32` wrapping ops).
fn hash32(view: &[u8], c_init: u32) -> u32 {
    let length = view.len();
    let mut a: u32 = 0x9e37_79b9;
    let mut b: u32 = 0x9e37_79b9;
    let mut c: u32 = c_init;
    let mut index = 0usize;

    // Process 12-byte blocks while `index <= length - 12`.
    // Guard against underflow when length < 12.
    if length >= 12 {
        let end = length - 12;
        while index <= end {
            a = a.wrapping_add(get_u32_le(view, index));
            b = b.wrapping_add(get_u32_le(view, index + 4));
            c = c.wrapping_add(get_u32_le(view, index + 8));
            let (na, nb, nc) = mix(a, b, c);
            a = na;
            b = nb;
            c = nc;
            index += 12;
        }
    }

    let remainder = length - index;

    // The first byte of c is reserved for the length.
    c = c.wrapping_add(length as u32);

    if remainder >= 4 {
        a = a.wrapping_add(get_u32_le(view, index));
        index += 4;

        if remainder >= 8 {
            b = b.wrapping_add(get_u32_le(view, index));
            index += 4;

            if remainder >= 9 {
                c = c.wrapping_add((view[index] as u32) << 8);
                index += 1;
            }
            if remainder >= 10 {
                c = c.wrapping_add((view[index] as u32) << 16);
                index += 1;
            }
            if remainder == 11 {
                c = c.wrapping_add((view[index] as u32) << 24);
            }
        } else {
            if remainder >= 5 {
                b = b.wrapping_add(view[index] as u32);
                index += 1;
            }
            if remainder >= 6 {
                b = b.wrapping_add((view[index] as u32) << 8);
                index += 1;
            }
            if remainder == 7 {
                b = b.wrapping_add((view[index] as u32) << 16);
            }
        }
    } else {
        if remainder >= 1 {
            a = a.wrapping_add(view[index] as u32);
            index += 1;
        }
        if remainder >= 2 {
            a = a.wrapping_add((view[index] as u32) << 8);
            index += 1;
        }
        if remainder == 3 {
            a = a.wrapping_add((view[index] as u32) << 16);
        }
    }

    mix(a, b, c).2
}

/// `digest.ts` `mix`. All ops are 32-bit wrapping; `>>>` is a logical shift on
/// u32 and `<<` wraps.
fn mix(mut a: u32, mut b: u32, mut c: u32) -> (u32, u32, u32) {
    a = a.wrapping_sub(b);
    a = a.wrapping_sub(c);
    a ^= c >> 13;
    b = b.wrapping_sub(c);
    b = b.wrapping_sub(a);
    b ^= a << 8;
    c = c.wrapping_sub(a);
    c = c.wrapping_sub(b);
    c ^= b >> 13;
    a = a.wrapping_sub(b);
    a = a.wrapping_sub(c);
    a ^= c >> 12;
    b = b.wrapping_sub(c);
    b = b.wrapping_sub(a);
    b ^= a << 16;
    c = c.wrapping_sub(a);
    c = c.wrapping_sub(b);
    c ^= b >> 5;
    a = a.wrapping_sub(b);
    a = a.wrapping_sub(c);
    a ^= c >> 3;
    b = b.wrapping_sub(c);
    b = b.wrapping_sub(a);
    b ^= a << 10;
    c = c.wrapping_sub(a);
    c = c.wrapping_sub(b);
    c ^= b >> 15;
    (a, b, c)
}

/// Read a little-endian u32 at `offset`, matching `DataView.getUint32(_, true)`.
/// All in-bounds reads (Angular only reads where bytes exist) are exact.
#[inline]
fn get_u32_le(view: &[u8], offset: usize) -> u32 {
    (view[offset] as u32)
        | ((view[offset + 1] as u32) << 8)
        | ((view[offset + 2] as u32) << 16)
        | ((view[offset + 3] as u32) << 24)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known Angular fixture value. Angular's published `$localize` message id
    // for the bare text "Hello" (computeMsgId('Hello', '')) is this 63-bit
    // decimal. Pinning it verifies the fingerprint/mix port byte-for-byte
    // against Angular, not merely determinism.
    #[test]
    fn compute_msg_id_known_value_hello() {
        assert_eq!(compute_msg_id("Hello", ""), "3902961887793684628");
        // Adding a meaning rotates+adds the meaning fingerprint.
        assert_eq!(compute_msg_id("Hello", "greeting"), "5905004912418243898");
    }

    #[test]
    fn compute_msg_id_is_deterministic_and_decimal() {
        let id = compute_msg_id("Hello, World!", "");
        // Stable across calls.
        assert_eq!(id, compute_msg_id("Hello, World!", ""));
        // Decimal string, non-empty.
        assert!(!id.is_empty());
        assert!(id.chars().all(|c| c.is_ascii_digit()));
        // Fits in 63 bits.
        let v: u64 = id.parse().unwrap();
        assert!(v < (1u64 << 63));
    }

    #[test]
    fn compute_msg_id_meaning_changes_id() {
        let without = compute_msg_id("Hello", "");
        let with = compute_msg_id("Hello", "greeting");
        assert_ne!(without, with);
        assert!(with.chars().all(|c| c.is_ascii_digit()));
    }
}
