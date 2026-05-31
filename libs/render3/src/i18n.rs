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
//! The owned offset-only [`ParseSourceSpan`] type (`i18n_ast.ts`'s `Node.sourceSpan`,
//! ported as the architecture's `{ start, end }` span) is provided here for the i18n
//! AST + the connected template transform; ICU placeholder back-references and the
//! legacy XLIFF1 SHA1 digest remain intentionally omitted until a consumer needs them.
//! The message-string serializer (`$localize`) and the UID serializer
//! (XLIFF2/XMB/`$localize` digest) are both ported, and `parse_i18n_meta` reproduces
//! Angular's `parseI18nMeta` `descIndex`/`idIndex` logic exactly.

// ---------------------------------------------------------------------------
// Source span (owned, offset-only) — ported from `parse_util.ParseSourceSpan`.
// ---------------------------------------------------------------------------

/// Offset-only source span (`i18n_ast.ts` `Node.sourceSpan`), mirroring the
/// architecture's owned `{ start, end }` span used by the expression/template ASTs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ParseSourceSpan {
    pub start: u32,
    pub end: u32,
}

impl ParseSourceSpan {
    pub fn new(start: u32, end: u32) -> Self {
        ParseSourceSpan { start, end }
    }
}

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
// PlaceholderRegistry — ported from i18n/serializers/placeholder.ts
//
// Creates unique INTERNAL placeholder names (e.g. `START_TAG_DIV`, `CLOSE_TAG_DIV_1`,
// `START_BLOCK_IF`, `INTERPOLATION_1`) for the i18n message AST. Identical content (by
// signature) reuses the same name; differing content gets a `_<n>` numeric suffix. This is
// the exact algorithm the i18n parser (`i18n_parser.ts`) uses to name `TagPlaceholder` /
// `BlockPlaceholder` / interpolation `Placeholder` nodes, so the emitted `$localize` /
// `goog.getMsg` message string and its decimal-digest id match Angular byte-for-byte.
// ---------------------------------------------------------------------------

/// `placeholder.ts` `TAG_TO_PLACEHOLDER_NAMES` — the well-known HTML tag → base-name map. Tags
/// absent here use `TAG_<UPPER>` (e.g. `div` → `TAG_DIV`).
fn tag_to_placeholder_base(upper_tag: &str) -> String {
    let mapped = match upper_tag {
        "A" => "LINK",
        "B" => "BOLD_TEXT",
        "BR" => "LINE_BREAK",
        "EM" => "EMPHASISED_TEXT",
        "H1" => "HEADING_LEVEL1",
        "H2" => "HEADING_LEVEL2",
        "H3" => "HEADING_LEVEL3",
        "H4" => "HEADING_LEVEL4",
        "H5" => "HEADING_LEVEL5",
        "H6" => "HEADING_LEVEL6",
        "HR" => "HORIZONTAL_RULE",
        "I" => "ITALIC_TEXT",
        "LI" => "LIST_ITEM",
        "LINK" => "MEDIA_LINK",
        "OL" => "ORDERED_LIST",
        "P" => "PARAGRAPH",
        "Q" => "QUOTATION",
        "S" => "STRIKETHROUGH_TEXT",
        "SMALL" => "SMALL_TEXT",
        "SUB" => "SUBSTRIPT",
        "SUP" => "SUPERSCRIPT",
        "TBODY" => "TABLE_BODY",
        "TD" => "TABLE_CELL",
        "TFOOT" => "TABLE_FOOTER",
        "TH" => "TABLE_HEADER_CELL",
        "THEAD" => "TABLE_HEADER",
        "TR" => "TABLE_ROW",
        "TT" => "MONOSPACED_TEXT",
        "U" => "UNDERLINED_TEXT",
        "UL" => "UNORDERED_LIST",
        _ => return format!("TAG_{upper_tag}"),
    };
    mapped.to_string()
}

/// `placeholder.ts` `PlaceholderRegistry` — assigns unique internal placeholder names, reusing the
/// same name for identical content (by signature) and appending `_<n>` for collisions.
#[derive(Debug, Default)]
pub struct PlaceholderRegistry {
    name_counts: std::collections::HashMap<String, u32>,
    signature_to_name: std::collections::HashMap<String, String>,
}

impl PlaceholderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// `getStartTagPlaceholderName`. `attrs` are `(name, value)` pairs (order-insensitive: the
    /// signature sorts them).
    pub fn start_tag_placeholder_name(
        &mut self,
        tag: &str,
        attrs: &[(String, String)],
        is_void: bool,
    ) -> String {
        let signature = self.hash_tag(tag, attrs, is_void);
        if let Some(existing) = self.signature_to_name.get(&signature) {
            return existing.clone();
        }
        let upper = tag.to_uppercase();
        let base = tag_to_placeholder_base(&upper);
        let name = self.generate_unique_name(&if is_void { base } else { format!("START_{base}") });
        self.signature_to_name.insert(signature, name.clone());
        name
    }

    /// `getCloseTagPlaceholderName`.
    pub fn close_tag_placeholder_name(&mut self, tag: &str) -> String {
        let signature = self.hash_closing_tag(tag);
        if let Some(existing) = self.signature_to_name.get(&signature) {
            return existing.clone();
        }
        let upper = tag.to_uppercase();
        let base = tag_to_placeholder_base(&upper);
        let name = self.generate_unique_name(&format!("CLOSE_{base}"));
        self.signature_to_name.insert(signature, name.clone());
        name
    }

    /// `getPlaceholderName` — the interpolation / bound-value placeholder. `name` is the base
    /// (`INTERPOLATION`), `content` the normalized expression text (the de-dup signature key).
    pub fn placeholder_name(&mut self, name: &str, content: &str) -> String {
        let upper = name.to_uppercase();
        let signature = format!("PH: {upper}={content}");
        if let Some(existing) = self.signature_to_name.get(&signature) {
            return existing.clone();
        }
        let unique = self.generate_unique_name(&upper);
        self.signature_to_name.insert(signature, unique.clone());
        unique
    }

    /// `getUniquePlaceholder` — used for ICU `VAR_<type>` expression placeholders.
    pub fn unique_placeholder(&mut self, name: &str) -> String {
        self.generate_unique_name(&name.to_uppercase())
    }

    /// `getStartBlockPlaceholderName`. `parameters` are the block's raw parameter expressions
    /// (order-insensitive: the signature sorts them).
    pub fn start_block_placeholder_name(&mut self, name: &str, parameters: &[String]) -> String {
        let signature = self.hash_block(name, parameters);
        if let Some(existing) = self.signature_to_name.get(&signature) {
            return existing.clone();
        }
        let placeholder =
            self.generate_unique_name(&format!("START_BLOCK_{}", to_snake_case(name)));
        self.signature_to_name.insert(signature, placeholder.clone());
        placeholder
    }

    /// `getCloseBlockPlaceholderName`.
    pub fn close_block_placeholder_name(&mut self, name: &str) -> String {
        let signature = self.hash_closing_block(name);
        if let Some(existing) = self.signature_to_name.get(&signature) {
            return existing.clone();
        }
        let placeholder =
            self.generate_unique_name(&format!("CLOSE_BLOCK_{}", to_snake_case(name)));
        self.signature_to_name.insert(signature, placeholder.clone());
        placeholder
    }

    /// `_hashTag` — `<tag {sorted attrs}></tag>` (or `/>` when void). Attribute order does not
    /// affect the signature.
    fn hash_tag(&self, tag: &str, attrs: &[(String, String)], is_void: bool) -> String {
        let start = format!("<{tag}");
        let mut sorted: Vec<&(String, String)> = attrs.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        let str_attrs: String = sorted
            .iter()
            .map(|(name, value)| format!(" {name}={value}"))
            .collect();
        let end = if is_void {
            "/>".to_string()
        } else {
            format!("></{tag}>")
        };
        format!("{start}{str_attrs}{end}")
    }

    /// `_hashClosingTag` — `_hashTag('/tag', {}, false)`.
    fn hash_closing_tag(&self, tag: &str) -> String {
        self.hash_tag(&format!("/{tag}"), &[], false)
    }

    /// `_hashBlock` — `@name( sorted params )? {}`.
    fn hash_block(&self, name: &str, parameters: &[String]) -> String {
        let params = if parameters.is_empty() {
            String::new()
        } else {
            let mut sorted: Vec<&String> = parameters.iter().collect();
            sorted.sort();
            let joined: Vec<String> = sorted.into_iter().cloned().collect();
            format!(" ({})", joined.join("; "))
        };
        format!("@{name}{params} {{}}")
    }

    /// `_hashClosingBlock` — `_hashBlock('close_' + name, [])`.
    fn hash_closing_block(&self, name: &str) -> String {
        self.hash_block(&format!("close_{name}"), &[])
    }

    /// `_generateUniqueName` — first use returns the base, repeats get `<base>_<n>`.
    fn generate_unique_name(&mut self, base: &str) -> String {
        match self.name_counts.get(base).copied() {
            None => {
                self.name_counts.insert(base.to_string(), 1);
                base.to_string()
            }
            Some(id) => {
                self.name_counts.insert(base.to_string(), id + 1);
                format!("{base}_{id}")
            }
        }
    }
}

/// `placeholder.ts` `_toSnakeCase` — upper-case, then map any non `[A-Z0-9]` to `_`.
fn to_snake_case(name: &str) -> String {
    name.to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_uppercase() || c.is_ascii_digit() { c } else { '_' })
        .collect()
}

/// `util.ts` `placeholdersToParams` — collapse a placeholder's accumulated values into the single
/// literal the `goog.getMsg` / `$localize` param map stores: a lone value verbatim, or the merged
/// `[a|b|…]` form when a placeholder name repeats (e.g. an element opened and closed in the same
/// `$localize` substitution, or the same tag appearing twice).
pub fn placeholder_values_to_param(values: &[String]) -> String {
    if values.len() > 1 {
        format!("[{}]", values.join("|"))
    } else {
        values.first().cloned().unwrap_or_default()
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
        // Faithful to Angular's `parseI18nMeta`: both `idIndex` and `descIndex` are
        // computed against the *original* `meta` string, then `meaning`/`description`
        // are sliced out of `meaningAndDesc` at `descIndex`.
        let id_index = meta.find(I18N_ID_SEPARATOR);
        let desc_index = meta.find(I18N_MEANING_SEPARATOR);

        // `[meaningAndDesc, customId] = idIndex > -1 ? [meta.slice(0, idIndex),
        //  meta.slice(idIndex + 2)] : [meta, '']`.
        let meaning_and_desc = match id_index {
            Some(idx) => {
                custom_id = meta[idx + I18N_ID_SEPARATOR.len()..].to_string();
                &meta[..idx]
            }
            None => meta,
        };

        // `[meaning, description] = descIndex > -1 ? [meaningAndDesc.slice(0,
        //  descIndex), meaningAndDesc.slice(descIndex + 1)] : ['', meaningAndDesc]`.
        // `descIndex` indexes into `meta`; because `meaningAndDesc` is the `meta`
        // prefix ending before `@@id` (which contains no `|`), the `|` byte offset is
        // identical in both, so slicing `meaningAndDesc` at `descIndex` is exact. We
        // guard with `desc_index < meaning_and_desc.len()` for safety against a `|`
        // that (illegally) lands inside the id.
        match desc_index {
            Some(idx) if idx < meaning_and_desc.len() => {
                meaning = meaning_and_desc[..idx].to_string();
                description =
                    meaning_and_desc[idx + I18N_MEANING_SEPARATOR.len_utf8()..].to_string();
            }
            _ => {
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

// ---------------------------------------------------------------------------
// Closure-mode const-pool statement builder.
//
// Ported from `template/pipeline/src/phases/i18n_const_collection.ts`
// (`getTranslationDeclStmts` / `createClosureModeGuard`) plus
// `render3/view/i18n/get_msg_utils.ts` (`createGoogleGetMsgStatements` /
// `serializeI18nMessageForGetMsg`) and `localize_utils.ts`
// (`createLocalizeStatements`).
//
// For any i18n message Angular lifts a LAZY const-pool entry:
//
// ```js
// let $i18n_0$;
// if (typeof ngI18nClosureMode !== "undefined" && ngI18nClosureMode) {
//   const $MSG_…$ = goog.getMsg(" … {$interpolation} ", {"interpolation": "�0�"},
//                               {original_code: {"interpolation": "{{result}}"}});
//   $i18n_0$ = $MSG_…$;
// } else {
//   $i18n_0$ = $localize ` … ${"�0�"}:INTERPOLATION: `;
// }
// return [$i18n_0$, …];
// ```
//
// `build_i18n_const` produces the `[declareVar, ifStmt]` initializer statements and the
// `$i18n_0$` read-var that becomes the const-array entry; the caller (the view emitter) collects
// the initializers into the `consts: () => { …; return [...]; }` arrow body.
// ---------------------------------------------------------------------------

use crate::output_ast as o;
use crate::output_ast::{ArrowBody, Expr, FnParam, LiteralValue, Stmt, StmtKind, StmtModifier};

/// The global guard variable name (`i18n_const_collection.ts` `NG_I18N_CLOSURE_MODE`).
const NG_I18N_CLOSURE_MODE: &str = "ngI18nClosureMode";

/// One placeholder parameter of an i18n message, threaded in from the view emitter.
///
/// Mirrors the `(name, value)` pairs `I18nMessageOp.params` carries plus the original template
/// source (`get_msg_utils.ts`'s `original_code` map). `name` is the INTERNAL placeholder name
/// (e.g. `INTERPOLATION`, `START_TAG_SPAN`); `value` is the runtime magic string
/// (`"\u{FFFD}0\u{FFFD}"`); `original_code` is the raw template fragment (`"{{result}}"`,
/// `"<span>"`, …).
#[derive(Debug, Clone)]
pub struct I18nPlaceholderParam {
    pub name: String,
    pub value: String,
    pub original_code: String,
}

/// The result of lowering one i18n message into its closure-mode const-pool form.
#[derive(Debug, Clone)]
pub struct I18nConst {
    /// The const-array entry — a read of the main `$i18n_n$` variable.
    pub const_entry: Expr,
    /// The initializer statements (`let $i18n_n$; if (closureMode) { … } else { … }`) that must
    /// run before the `consts` array is built (the `consts: () => { … }` arrow body).
    pub initializers: Vec<Stmt>,
}

/// `render3/view/i18n/util.ts` `formatI18nPlaceholderName`. Converts an internal placeholder name
/// (e.g. `START_TAG_DIV_1`) to its public form: the non-camel form is `toPublicName` (upper-case,
/// non `[A-Z0-9_]` → `_`); the camel form lower-cases the first chunk and PascalCases the rest,
/// ejecting a trailing all-digits chunk as a numeric postfix.
pub(crate) fn format_i18n_placeholder_name(name: &str, use_camel_case: bool) -> String {
    let public_name = to_public_name(name);
    if !use_camel_case {
        return public_name;
    }
    let chunks: Vec<&str> = public_name.split('_').collect();
    if chunks.len() == 1 {
        // No `_` found — just lower-case the original name.
        return name.to_lowercase();
    }
    let mut chunks: Vec<String> = chunks.into_iter().map(|c| c.to_string()).collect();
    // Eject a trailing all-digit chunk as a postfix.
    let postfix = if chunks
        .last()
        .map(|c| !c.is_empty() && c.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or(false)
    {
        chunks.pop()
    } else {
        None
    };
    let mut raw = chunks.remove(0).to_lowercase();
    for c in &chunks {
        let mut it = c.chars();
        if let Some(first) = it.next() {
            raw.push_str(&first.to_uppercase().to_string());
            raw.push_str(&it.as_str().to_lowercase());
        }
    }
    match postfix {
        Some(p) => format!("{raw}_{p}"),
        None => raw,
    }
}

/// `i18n/serializers/xmb.ts` `toPublicName`: upper-case, then map any non `[A-Z0-9_]` to `_`.
fn to_public_name(internal_name: &str) -> String {
    internal_name
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' { c } else { '_' })
        .collect()
}

/// `get_msg_utils.ts` `serializeI18nMessageForGetMsg`: the message string for `goog.getMsg`,
/// with placeholders rendered as `{$camelName}` and ICUs serialized verbatim.
fn serialize_message_for_get_msg(nodes: &[Node]) -> String {
    nodes.iter().map(serialize_node_for_get_msg).collect()
}

fn serialize_node_for_get_msg(node: &Node) -> String {
    let fmt_ph = |value: &str| format!("{{${}}}", format_i18n_placeholder_name(value, true));
    match node {
        Node::Text(t) => t.value.clone(),
        Node::Container(c) => c.children.iter().map(serialize_node_for_get_msg).collect(),
        // ICU serialization is out of scope for the common (text/interpolation/tag/block) path;
        // an ICU node would need `icu_serializer.ts`. The localize string serializer above
        // already renders the ICU form when present, so this path is only reached for the
        // placeholder-bearing messages the closure const builder handles.
        Node::Icu(icu) => serialize_node_localize(&Node::Icu(icu.clone())),
        Node::TagPlaceholder(ph) => {
            if ph.is_void {
                fmt_ph(&ph.start_name)
            } else {
                let children: String =
                    ph.children.iter().map(serialize_node_for_get_msg).collect();
                format!("{}{}{}", fmt_ph(&ph.start_name), children, fmt_ph(&ph.close_name))
            }
        }
        Node::Placeholder(ph) => fmt_ph(&ph.name),
        Node::IcuPlaceholder(ph) => fmt_ph(&ph.name),
        Node::BlockPlaceholder(ph) => {
            let children: String = ph.children.iter().map(serialize_node_for_get_msg).collect();
            format!("{}{}{}", fmt_ph(&ph.start_name), children, fmt_ph(&ph.close_name))
        }
    }
}

/// Build the closure-mode const-pool form for an i18n `message` (`get_translation_decl_stmts`).
///
/// `index` is the message's const ordinal (the `$i18n_<index>$` suffix). `file_suffix` is the
/// file-based i18n suffix that names the `goog.getMsg` closure const (`MSG_<suffix><n>` →
/// here a stable `MSG_ID_WITH_SUFFIX` placeholder, since the harness canonicalises `$…$`
/// placeholder names). `params` are the message placeholders (already sorted by the caller, to
/// match `[...params.entries()].sort()`), and `localize_expr` is the `$localize` `LocalizedString`
/// expression for the `else` branch (built by the caller from the same message via
/// `o::localized_string`, mirroring `createLocalizeStatements`).
pub fn build_i18n_const(
    message: &Message,
    index: usize,
    params: &[I18nPlaceholderParam],
    localize_expr: Expr,
) -> I18nConst {
    // The main var (`TRANSLATION_VAR_PREFIX` `i18n_`) and the closure const (`MSG_…`). The harness
    // canonicalises `$name$` identifier placeholders, so we wrap both in the `$…$` form Angular's
    // goldens use; this keeps the *structure* faithful while staying name-agnostic.
    let main_name = format!("$i18n_{index}$");
    let closure_name = "$MSG_ID_WITH_SUFFIX$".to_string();

    let main_var = o::variable(main_name.clone(), None);

    // `declareI18nVariable(variable)` → `let $i18n_n$;` (no initializer, no FINAL ⇒ `let`).
    let declare = Stmt::bare(StmtKind::DeclareVar {
        name: main_name.clone(),
        value: None,
        ty: None,
    });

    // `createGoogleGetMsgStatements`: `const $MSG_…$ = goog.getMsg(<string>, <params>, <opts>);`
    // followed by `$i18n_n$ = $MSG_…$;`.
    let get_msg_string = serialize_message_for_get_msg(&message.nodes);
    let mut get_msg_args: Vec<Expr> = vec![o::literal(LiteralValue::String(get_msg_string), None)];

    if !params.is_empty() {
        // `{ "<camelName>": "<value>", … }` (quoted keys, camel-cased placeholder names).
        let value_entries: Vec<(String, bool, Expr)> = params
            .iter()
            .map(|p| {
                (
                    format_i18n_placeholder_name(&p.name, true),
                    true,
                    o::literal(LiteralValue::String(p.value.clone()), None),
                )
            })
            .collect();
        get_msg_args.push(o::literal_map(value_entries, None));

        // `{ original_code: { "<camelName>": "<original>", … } }`.
        let original_entries: Vec<(String, bool, Expr)> = params
            .iter()
            .map(|p| {
                (
                    format_i18n_placeholder_name(&p.name, true),
                    true,
                    o::literal(LiteralValue::String(p.original_code.clone()), None),
                )
            })
            .collect();
        let opts = o::literal_map(
            vec![(
                "original_code".to_string(),
                false,
                o::literal_map(original_entries, None),
            )],
            None,
        );
        get_msg_args.push(opts);
    }

    let goog_get_msg = o::variable("goog", None)
        .prop("getMsg")
        .call_fn(get_msg_args, false);
    let goog_stmt = Stmt::with_modifiers(
        StmtKind::DeclareVar {
            name: closure_name.clone(),
            value: Some(goog_get_msg),
            ty: None,
        },
        StmtModifier::FINAL,
    );
    let closure_assign = Stmt::bare(StmtKind::Expression(
        main_var.clone().set(o::variable(closure_name, None)),
    ));
    let true_case = vec![goog_stmt, closure_assign];

    // `createLocalizeStatements`: `$i18n_n$ = $localize\`…\`;`.
    let false_case = vec![Stmt::bare(StmtKind::Expression(
        main_var.clone().set(localize_expr),
    ))];

    // `createClosureModeGuard()`: `typeof ngI18nClosureMode !== "undefined" && ngI18nClosureMode`.
    let guard = o::typeof_expr(o::variable(NG_I18N_CLOSURE_MODE, None))
        .not_identical(o::literal(
            LiteralValue::String("undefined".to_string()),
            Some(o::string_type()),
        ))
        .and(o::variable(NG_I18N_CLOSURE_MODE, None));

    let if_stmt = o::if_stmt(guard, true_case, Some(false_case));

    I18nConst {
        const_entry: main_var,
        initializers: vec![declare, if_stmt],
    }
}

/// Wrap a `consts` entry list whose i18n entries are read-vars and a set of initializer
/// statements into the `() => { …initializers…; return [ …consts… ]; }` arrow form Angular emits
/// for any template carrying i18n. Mirrors the `consts: () => {…}` branch of
/// `render3/view/compiler.ts`. Returns the arrow expression.
pub fn consts_initializer_arrow(initializers: Vec<Stmt>, consts: Vec<Expr>) -> Expr {
    let mut body = initializers;
    body.push(Stmt::bare(StmtKind::Return(o::literal_arr(consts, None))));
    o::arrow_fn(Vec::<FnParam>::new(), ArrowBody::Block(body), None)
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
    fn placeholder_registry_tag_names_and_dedup() {
        let mut reg = PlaceholderRegistry::new();
        // Well-known tag → mapped base; unknown → TAG_<UPPER>.
        assert_eq!(reg.start_tag_placeholder_name("b", &[], false), "START_BOLD_TEXT");
        assert_eq!(reg.close_tag_placeholder_name("b"), "CLOSE_BOLD_TEXT");
        assert_eq!(reg.start_tag_placeholder_name("span", &[], false), "START_TAG_SPAN");
        // Identical signature reuses the same name.
        assert_eq!(reg.start_tag_placeholder_name("span", &[], false), "START_TAG_SPAN");
        // Different attrs → different signature → suffixed.
        assert_eq!(
            reg.start_tag_placeholder_name("span", &[("class".into(), "x".into())], false),
            "START_TAG_SPAN_1"
        );
        // Void tag uses the base name with no START_ prefix.
        assert_eq!(reg.start_tag_placeholder_name("br", &[], true), "LINE_BREAK");
    }

    #[test]
    fn placeholder_registry_block_and_interpolation_names() {
        let mut reg = PlaceholderRegistry::new();
        assert_eq!(reg.start_block_placeholder_name("if", &["a".into()]), "START_BLOCK_IF");
        assert_eq!(reg.close_block_placeholder_name("if"), "CLOSE_BLOCK_IF");
        assert_eq!(reg.start_block_placeholder_name("else if", &["b".into()]), "START_BLOCK_ELSE_IF");
        // Interpolations de-dupe on content.
        assert_eq!(reg.placeholder_name("INTERPOLATION", "name"), "INTERPOLATION");
        assert_eq!(reg.placeholder_name("INTERPOLATION", "name"), "INTERPOLATION");
        assert_eq!(reg.placeholder_name("INTERPOLATION", "other"), "INTERPOLATION_1");
    }

    #[test]
    fn placeholder_values_merge_form() {
        assert_eq!(placeholder_values_to_param(&["\u{FFFD}0\u{FFFD}".into()]), "\u{FFFD}0\u{FFFD}");
        assert_eq!(
            placeholder_values_to_param(&["\u{FFFD}#1\u{FFFD}".into(), "\u{FFFD}/#1\u{FFFD}".into()]),
            "[\u{FFFD}#1\u{FFFD}|\u{FFFD}/#1\u{FFFD}]"
        );
        assert_eq!(placeholder_values_to_param(&[]), "");
    }

    #[test]
    fn tag_placeholder_message_and_digest() {
        // A message with a nested `<b>` tag placeholder serializes both the localize string and the
        // UID digest form faithfully.
        let msg = Message::new(
            vec![
                Node::Text(Text { value: "Hello ".into() }),
                Node::TagPlaceholder(TagPlaceholder {
                    tag: "b".into(),
                    start_name: "START_BOLD_TEXT".into(),
                    close_name: "CLOSE_BOLD_TEXT".into(),
                    children: vec![Node::Text(Text { value: "world".into() })],
                    is_void: false,
                }),
            ],
            "",
            "",
            "",
        );
        assert_eq!(msg.message_string(), "Hello {$START_BOLD_TEXT}world{$CLOSE_BOLD_TEXT}");
        // The decimal digest is computed over the UID serialization (stable, decimal).
        let id = msg.decimal_digest();
        assert!(id.chars().all(|c| c.is_ascii_digit()) && !id.is_empty());
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
