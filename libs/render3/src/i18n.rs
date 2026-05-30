//! i18n CORE infrastructure.
//!
//! Ported from Angular 22.1's `packages/compiler/src/i18n` (vendored at
//! `tools/angular-ref`):
//!   - `digest.ts`            -> [`compute_msg_id`], [`fingerprint`], `hash32`, `mix`
//!   - `i18n_ast.ts`          -> [`Message`], [`Node`] (owned Rust enums/structs)
//!   - `render3/view/i18n/meta.ts::parseI18nMeta` -> [`parse_i18n_meta`], [`I18nMeta`]
//!
//! This module is self-contained: it provides the data structures + algorithms
//! (message-id digest, i18n meta parsing) needed to represent a parsed i18n
//! message and compute its `$localize` id. Wiring into the template transform
//! is deferred to a later round.
//!
//! NOTE(port): The full i18n AST in Angular carries `ParseSourceSpan`s on every
//! node and a `Visitor` trait with many concrete visitors (Clone/Recurse/
//! Serializer). Here we port only the owned node shapes plus the two
//! serializers that the digest needs (the message-string serializer used for
//! `$localize` and the UID serializer used by the XLIFF2/XMB/$localize digest).
//! Source spans, ICU placeholder back-references, and legacy XLIFF1 SHA1 digest
//! are intentionally omitted until a consumer needs them.

// ---------------------------------------------------------------------------
// i18n AST (owned)  — ported from i18n_ast.ts
// ---------------------------------------------------------------------------

/// An i18n message AST node.
///
/// Owned port of the `Node` hierarchy from `i18n_ast.ts`. Each variant mirrors
/// one of Angular's `Text` / `Container` / `Icu` / `TagPlaceholder` /
/// `Placeholder` / `IcuPlaceholder` / `BlockPlaceholder` classes.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// Literal text.
    Text(Text),
    /// A grouping of child nodes (no rendered marker of its own).
    Container(Container),
    /// An ICU expression (`{count, plural, ...}`).
    Icu(Icu),
    /// A placeholder for an element open/close tag pair (or a void element).
    TagPlaceholder(TagPlaceholder),
    /// A placeholder for an interpolation / bound value.
    Placeholder(Placeholder),
    /// A placeholder standing in for a nested ICU.
    IcuPlaceholder(IcuPlaceholder),
    /// A placeholder for a control-flow block open/close pair.
    BlockPlaceholder(BlockPlaceholder),
}

/// Literal text node. (`i18n_ast.ts` `Text`.)
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub value: String,
}

/// A grouping of child nodes. (`i18n_ast.ts` `Container`.)
#[derive(Debug, Clone, PartialEq)]
pub struct Container {
    pub children: Vec<Node>,
}

/// An ICU expression. (`i18n_ast.ts` `Icu`.)
///
/// `cases` preserves insertion order (Angular iterates `Object.keys(cases)`,
/// which for string keys is insertion order), so we use an ordered `Vec` of
/// `(case_key, node)` pairs rather than a map.
#[derive(Debug, Clone, PartialEq)]
pub struct Icu {
    pub expression: String,
    pub icu_type: String,
    pub cases: Vec<(String, Node)>,
    /// The placeholder name used for `expression` in the `$localize` string.
    pub expression_placeholder: Option<String>,
}

/// A placeholder for an element tag. (`i18n_ast.ts` `TagPlaceholder`.)
#[derive(Debug, Clone, PartialEq)]
pub struct TagPlaceholder {
    pub tag: String,
    pub start_name: String,
    pub close_name: String,
    pub children: Vec<Node>,
    pub is_void: bool,
}

/// A placeholder for a bound/interpolated value. (`i18n_ast.ts` `Placeholder`.)
#[derive(Debug, Clone, PartialEq)]
pub struct Placeholder {
    pub value: String,
    pub name: String,
}

/// A placeholder standing in for a nested ICU. (`i18n_ast.ts` `IcuPlaceholder`.)
#[derive(Debug, Clone, PartialEq)]
pub struct IcuPlaceholder {
    pub value: Icu,
    pub name: String,
}

/// A placeholder for a control-flow block. (`i18n_ast.ts` `BlockPlaceholder`.)
#[derive(Debug, Clone, PartialEq)]
pub struct BlockPlaceholder {
    pub name: String,
    pub parameters: Vec<String>,
    pub start_name: String,
    pub close_name: String,
    pub children: Vec<Node>,
}

/// A parsed i18n message. (`i18n_ast.ts` `Message`.)
///
/// `id` is initialized to `custom_id` (matching `Message` whose `id` defaults to
/// the `customId`). When the custom id is empty, the effective id is computed
/// lazily via [`Message::decimal_digest`].
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub nodes: Vec<Node>,
    pub meaning: String,
    pub description: String,
    pub custom_id: String,
}

impl Message {
    /// Construct a message, mirroring the `Message` constructor's field setup.
    pub fn new(
        nodes: Vec<Node>,
        meaning: impl Into<String>,
        description: impl Into<String>,
        custom_id: impl Into<String>,
    ) -> Self {
        Message {
            nodes,
            meaning: meaning.into(),
            description: description.into(),
            custom_id: custom_id.into(),
        }
    }

    /// The `$localize` message string (`i18n_ast.ts` `serializeMessage`).
    pub fn message_string(&self) -> String {
        serialize_message(&self.nodes)
    }

    /// Return the custom id if present, else compute the XLIFF2/XMB/$localize
    /// decimal digest (`digest.ts` `decimalDigest`).
    pub fn decimal_digest(&self) -> String {
        if !self.custom_id.is_empty() {
            return self.custom_id.clone();
        }
        self.compute_decimal_digest()
    }

    /// `digest.ts` `computeDecimalDigest`: serialize nodes ignoring ICU
    /// expressions, then `computeMsgId`.
    pub fn compute_decimal_digest(&self) -> String {
        let parts: String = self
            .nodes
            .iter()
            .map(serialize_node_for_uid)
            .collect();
        compute_msg_id(&parts, &self.meaning)
    }
}

// ---------------------------------------------------------------------------
// Serializers — ported from i18n_ast.ts / digest.ts
// ---------------------------------------------------------------------------

/// `i18n_ast.ts` `LocalizeMessageStringVisitor` — the `$localize` string form.
fn serialize_message(nodes: &[Node]) -> String {
    nodes.iter().map(serialize_node_localize).collect()
}

fn serialize_node_localize(node: &Node) -> String {
    match node {
        Node::Text(t) => t.value.clone(),
        Node::Container(c) => c.children.iter().map(serialize_node_localize).collect(),
        Node::Icu(icu) => {
            let cases: Vec<String> = icu
                .cases
                .iter()
                .map(|(k, n)| format!("{} {{{}}}", k, serialize_node_localize(n)))
                .collect();
            format!(
                "{{{}, {}, {}}}",
                icu.expression_placeholder.as_deref().unwrap_or(""),
                icu.icu_type,
                cases.join(" ")
            )
        }
        Node::TagPlaceholder(ph) => {
            let children: String = ph.children.iter().map(serialize_node_localize).collect();
            format!("{{${}}}{}{{${}}}", ph.start_name, children, ph.close_name)
        }
        Node::Placeholder(ph) => format!("{{${}}}", ph.name),
        Node::IcuPlaceholder(ph) => format!("{{${}}}", ph.name),
        Node::BlockPlaceholder(ph) => {
            let children: String = ph.children.iter().map(serialize_node_localize).collect();
            format!("{{${}}}{}{{${}}}", ph.start_name, children, ph.close_name)
        }
    }
}

/// `digest.ts` `_SerializerIgnoreIcuExpVisitor` — the UID serialization used by
/// the decimal digest. It is `_SerializerVisitor` except ICU drops the
/// expression.
fn serialize_node_for_uid(node: &Node) -> String {
    match node {
        Node::Text(t) => t.value.clone(),
        Node::Container(c) => {
            let parts: Vec<String> = c.children.iter().map(serialize_node_for_uid).collect();
            format!("[{}]", parts.join(", "))
        }
        Node::Icu(icu) => {
            // IgnoreIcuExp: do not include the expression.
            let cases: Vec<String> = icu
                .cases
                .iter()
                .map(|(k, n)| format!("{} {{{}}}", k, serialize_node_for_uid(n)))
                .collect();
            format!("{{{}, {}}}", icu.icu_type, cases.join(", "))
        }
        Node::TagPlaceholder(ph) => {
            if ph.is_void {
                format!("<ph tag name=\"{}\"/>", ph.start_name)
            } else {
                let children: Vec<String> =
                    ph.children.iter().map(serialize_node_for_uid).collect();
                format!(
                    "<ph tag name=\"{}\">{}</ph name=\"{}\">",
                    ph.start_name,
                    children.join(", "),
                    ph.close_name
                )
            }
        }
        Node::Placeholder(ph) => {
            if !ph.value.is_empty() {
                format!("<ph name=\"{}\">{}</ph>", ph.name, ph.value)
            } else {
                format!("<ph name=\"{}\"/>", ph.name)
            }
        }
        Node::IcuPlaceholder(ph) => {
            format!(
                "<ph icu name=\"{}\">{}</ph>",
                ph.name,
                serialize_node_for_uid(&Node::Icu(ph.value.clone()))
            )
        }
        Node::BlockPlaceholder(ph) => {
            let children: Vec<String> = ph.children.iter().map(serialize_node_for_uid).collect();
            format!(
                "<ph block name=\"{}\">{}</ph name=\"{}\">",
                ph.start_name,
                children.join(", "),
                ph.close_name
            )
        }
    }
}

// ---------------------------------------------------------------------------
// i18n meta parsing — ported from render3/view/i18n/meta.ts
// ---------------------------------------------------------------------------

const I18N_MEANING_SEPARATOR: char = '|';
const I18N_ID_SEPARATOR: &str = "@@";

/// Parsed i18n meta string. (`render3/view/i18n/meta.ts` `I18nMeta`, fields
/// `customId` / `meaning` / `description`.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct I18nMeta {
    pub custom_id: String,
    pub meaning: String,
    pub description: String,
}

/// `render3/view/i18n/meta.ts` `parseI18nMeta`.
///
/// Parses i18n metas like:
///  - `"@@id"`
///  - `"description[@@id]"`
///  - `"meaning|description[@@id]"`
pub fn parse_i18n_meta(meta: &str) -> I18nMeta {
    let mut custom_id = String::new();
    let mut meaning = String::new();
    let mut description = String::new();

    let meta = meta.trim();
    if !meta.is_empty() {
        // Split off the `@@id` (everything after the first `@@`).
        let (meaning_and_desc, id) = match meta.find(I18N_ID_SEPARATOR) {
            Some(idx) => (&meta[..idx], &meta[idx + I18N_ID_SEPARATOR.len()..]),
            None => (meta, ""),
        };
        custom_id = id.to_string();

        // NOTE(port): Angular computes `descIndex` against the *original* `meta`
        // string but then slices `meaningAndDesc`. With a `|` present this is
        // equivalent because `@@id` (containing no `|`) always trails the
        // meaning/description, so the index of `|` is identical in both. We
        // search `meaning_and_desc` directly, which is equivalent for all valid
        // inputs and avoids a panic on a `|` that lands inside the id.
        match meaning_and_desc.find(I18N_MEANING_SEPARATOR) {
            Some(idx) => {
                meaning = meaning_and_desc[..idx].to_string();
                description = meaning_and_desc[idx + I18N_MEANING_SEPARATOR.len_utf8()..].to_string();
            }
            None => {
                description = meaning_and_desc.to_string();
            }
        }
    }

    I18nMeta {
        custom_id,
        meaning,
        description,
    }
}

// ---------------------------------------------------------------------------
// Message-id digest — ported from digest.ts (fingerprint / computeMsgId)
// ---------------------------------------------------------------------------

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

    #[test]
    fn parse_i18n_meta_full() {
        let m = parse_i18n_meta("meaning|desc@@id");
        assert_eq!(m.meaning, "meaning");
        assert_eq!(m.description, "desc");
        assert_eq!(m.custom_id, "id");
    }

    #[test]
    fn parse_i18n_meta_id_only() {
        let m = parse_i18n_meta("@@custom-id");
        assert_eq!(m.meaning, "");
        assert_eq!(m.description, "");
        assert_eq!(m.custom_id, "custom-id");
    }

    #[test]
    fn parse_i18n_meta_desc_and_id() {
        let m = parse_i18n_meta("description@@id");
        assert_eq!(m.meaning, "");
        assert_eq!(m.description, "description");
        assert_eq!(m.custom_id, "id");
    }

    #[test]
    fn parse_i18n_meta_meaning_and_desc_no_id() {
        let m = parse_i18n_meta("meaning|description");
        assert_eq!(m.meaning, "meaning");
        assert_eq!(m.description, "description");
        assert_eq!(m.custom_id, "");
    }

    #[test]
    fn parse_i18n_meta_desc_only() {
        let m = parse_i18n_meta("just a description");
        assert_eq!(m.meaning, "");
        assert_eq!(m.description, "just a description");
        assert_eq!(m.custom_id, "");
    }

    #[test]
    fn parse_i18n_meta_empty_and_whitespace() {
        let m = parse_i18n_meta("");
        assert_eq!(m, I18nMeta::default());
        let m = parse_i18n_meta("   ");
        assert_eq!(m, I18nMeta::default());
        // trimmed
        let m = parse_i18n_meta("  desc@@id  ");
        assert_eq!(m.description, "desc");
        assert_eq!(m.custom_id, "id");
    }

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

    #[test]
    fn message_decimal_digest_uses_custom_id() {
        let msg = Message::new(
            vec![Node::Text(Text {
                value: "Hello".into(),
            })],
            "",
            "",
            "myCustomId",
        );
        assert_eq!(msg.decimal_digest(), "myCustomId");
    }

    #[test]
    fn message_decimal_digest_computed_matches_compute_msg_id() {
        let msg = Message::new(
            vec![Node::Text(Text {
                value: "Hello".into(),
            })],
            "",
            "",
            "",
        );
        // For a single text node the UID serialization is just the text.
        assert_eq!(msg.decimal_digest(), compute_msg_id("Hello", ""));
        assert_eq!(msg.message_string(), "Hello");
    }

    #[test]
    fn message_string_placeholder_form() {
        let msg = Message::new(
            vec![
                Node::Text(Text {
                    value: "Hello ".into(),
                }),
                Node::Placeholder(Placeholder {
                    value: "{{name}}".into(),
                    name: "INTERPOLATION".into(),
                }),
            ],
            "",
            "",
            "",
        );
        assert_eq!(msg.message_string(), "Hello {$INTERPOLATION}");
    }
}
