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
// I18nParamValue + formatValue/formatParamValues
//
// Ported from `template/pipeline/src/phases/extract_i18n_messages.ts`
// (`formatValue`, `formatParamValues`) plus the `I18nParamValueFlags` enum
// (`ir/src/enums.ts`). A `TagPlaceholder` / `BlockPlaceholder`'s runtime
// substitution value is the sentinel `�{closeMarker}{tagMarker}{slot}{:subTemplateIndex}�`,
// where the tag marker is `#` for an element and `*` for a template/block, the close
// marker is `/`, and the context (`:n`) is the sub-template index (null at root).
// ---------------------------------------------------------------------------

/// `ir/src/enums.ts` `I18nParamValueFlags` — the bit flags encoding how a placeholder's
/// runtime value is serialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct I18nParamValueFlags(pub u8);

impl I18nParamValueFlags {
    pub const NONE: I18nParamValueFlags = I18nParamValueFlags(0b0000);
    pub const ELEMENT_TAG: I18nParamValueFlags = I18nParamValueFlags(0b0001);
    pub const TEMPLATE_TAG: I18nParamValueFlags = I18nParamValueFlags(0b0010);
    pub const OPEN_TAG: I18nParamValueFlags = I18nParamValueFlags(0b0100);
    pub const CLOSE_TAG: I18nParamValueFlags = I18nParamValueFlags(0b1000);

    pub fn contains(self, other: I18nParamValueFlags) -> bool {
        (self.0 & other.0) != 0
    }

    pub fn without(self, other: I18nParamValueFlags) -> I18nParamValueFlags {
        I18nParamValueFlags(self.0 & !other.0)
    }
}

impl std::ops::BitOr for I18nParamValueFlags {
    type Output = I18nParamValueFlags;
    fn bitor(self, rhs: I18nParamValueFlags) -> I18nParamValueFlags {
        I18nParamValueFlags(self.0 | rhs.0)
    }
}

/// One placeholder runtime value (`ir.I18nParamValue`). `value` is the data slot of the
/// element/template the placeholder points at; `sub_template_index` is the child-view index
/// (`None` at root); `flags` encode the tag/close markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct I18nParamValue {
    pub value: usize,
    pub sub_template_index: Option<usize>,
    pub flags: I18nParamValueFlags,
}

/// `extract_i18n_messages.ts` `formatValue` — serialize one `I18nParamValue` into its runtime
/// magic-string form.
pub fn format_value(value: &I18nParamValue) -> String {
    const ESCAPE: char = '\u{FFFD}';
    const ELEMENT_MARKER: &str = "#";
    const TEMPLATE_MARKER: &str = "*";
    const TAG_CLOSE_MARKER: &str = "/";

    // Self-closing tags concatenate the start and close tag values.
    if value.flags.contains(I18nParamValueFlags::OPEN_TAG)
        && value.flags.contains(I18nParamValueFlags::CLOSE_TAG)
    {
        let open = I18nParamValue {
            flags: value.flags.without(I18nParamValueFlags::CLOSE_TAG),
            ..value.clone()
        };
        let close = I18nParamValue {
            flags: value.flags.without(I18nParamValueFlags::OPEN_TAG),
            ..value.clone()
        };
        return format!("{}{}", format_value(&open), format_value(&close));
    }

    if value.flags == I18nParamValueFlags::NONE {
        return value.value.to_string();
    }

    let mut tag_marker = "";
    let mut close_marker = "";
    if value.flags.contains(I18nParamValueFlags::ELEMENT_TAG) {
        tag_marker = ELEMENT_MARKER;
    } else if value.flags.contains(I18nParamValueFlags::TEMPLATE_TAG) {
        tag_marker = TEMPLATE_MARKER;
    }
    if !tag_marker.is_empty() && value.flags.contains(I18nParamValueFlags::CLOSE_TAG) {
        close_marker = TAG_CLOSE_MARKER;
    }
    let context = match value.sub_template_index {
        None => String::new(),
        Some(idx) => format!(":{idx}"),
    };
    format!("{ESCAPE}{close_marker}{tag_marker}{}{context}{ESCAPE}", value.value)
}

/// `extract_i18n_messages.ts` `formatParamValues` — serialize an `I18nParamValue[]` into a single
/// string (lone value verbatim, or the merged `[a|b|…]` form for >1).
pub fn format_param_values(values: &[I18nParamValue]) -> Option<String> {
    if values.is_empty() {
        return None;
    }
    let serialized: Vec<String> = values.iter().map(format_value).collect();
    Some(if serialized.len() == 1 {
        serialized.into_iter().next().unwrap()
    } else {
        format!("[{}]", serialized.join("|"))
    })
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
// Message-id digest — moved to `core::digest` to break the core<-template cycle
// (the emitter calls these primitives; see TREATY-IVY-CRATE-SPLIT-PLAN §3).
// Re-exported here for byte-compat with the historical `crate::i18n::compute_msg_id`
// / `crate::i18n::fingerprint` surface.
// ---------------------------------------------------------------------------

pub use treaty_ivy_core::digest::{compute_msg_id, fingerprint};

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

use crate::identifiers::R3;
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

/// Context-dependent emit options for [`build_i18n_const`], mirroring the two ways Angular's own
/// compiler varies the const-pool form for a single i18n message.
#[derive(Debug, Clone, Default)]
pub struct I18nConstOpts {
    /// Retained for source compatibility; the real `@angular/compiler` emitter ALWAYS spells the
    /// translation variable as the bare `i18n_<index>` const (`TRANSLATION_VAR_PREFIX` +
    /// `pool.uniqueName` index), so this flag no longer changes the emitted name. (The earlier
    /// `$i18n_<index>$` placeholder form existed only to match the `__i18nMsg__` macro compliance
    /// goldens, which the harness skips — never a real Ivy emit.)
    pub bare_name: bool,
    /// Append `i18n_<index> = ɵɵi18nPostprocess(i18n_<index>);` after the closure guard, as Angular
    /// does whenever a placeholder value merges more than one position (`[�…�|�…�]`).
    pub needs_postprocess: bool,
    /// Angular's `fileBasedI18nSuffix`: `relativeContextFilePath.replace(/[^A-Za-z0-9]/g, '_')`
    /// upper-cased + `'_'`. The closure const name is `getTranslationConstPrefix(suffix)` =
    /// `('MSG_' + suffix).toUpperCase()` + `pool.uniqueName` index — e.g. an empty file path yields
    /// suffix `"_"` → prefix `"MSG__"` → `MSG__0` (matching `@angular/compiler`).
    pub file_suffix: String,
}

/// Angular's `fileBasedI18nSuffix` (`render3/view/i18n/util.ts`): take the component's
/// `relativeContextFilePath`, replace every non-alphanumeric byte with `_`, upper-case, and append a
/// trailing `_`. Feeds [`I18nConstOpts::file_suffix`] so the closure const name matches the oracle
/// (`MSG_<SUFFIX>_<index>`; an empty path → suffix `"_"` → `MSG__<index>`).
pub fn file_based_i18n_suffix(relative_context_file_path: &str) -> String {
    let sanitized: String = relative_context_file_path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{}_", sanitized.to_uppercase())
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
/// `index` is the message's const ordinal (the `i18n_<index>` suffix). `params` are the message
/// placeholders (already sorted by the caller, to match `[...params.entries()].sort()`), and
/// `localize_expr` is the `$localize` `LocalizedString` expression for the `else` branch (built by
/// the caller from the same message via `o::localized_string`, mirroring `createLocalizeStatements`).
///
/// `opts` selects the two context-dependent emit forms Angular itself varies on (the goldens prove
/// the divergence): the translation variable is written BARE `i18n_<index>` when the message
/// brackets control-flow blocks (`@if`/`@switch`/`@for`/`@defer` — Angular's `ts.Printer` emits the
/// genuine const identifier there) and as the `$i18n_<index>$` golden-placeholder form otherwise,
/// and a trailing `i18n_<index> = ɵɵi18nPostprocess(i18n_<index>);` is appended iff
/// [`I18nConstOpts::needs_postprocess`] (the message has a placeholder whose value merges >1
/// position into the `[a|b|…]` form, which the runtime must reorder).
pub fn build_i18n_const(
    message: &Message,
    index: usize,
    params: &[I18nPlaceholderParam],
    localize_expr: Expr,
    opts: I18nConstOpts,
) -> I18nConst {
    // The main var (`TRANSLATION_VAR_PREFIX` `i18n_` + `pool.uniqueName` index) and the closure
    // const (`getTranslationConstPrefix(fileBasedI18nSuffix)` + index). Both are byte-exact with
    // `@angular/compiler`'s real emitter: `let i18n_0; … const MSG__0 = goog.getMsg(…);`. (The
    // `$i18n_0$` / `$MSG_ID_WITH_SUFFIX$` placeholder spellings the harness once folded were only
    // for the `__i18nMsg__` macro goldens — which the compliance harness skips, not a real emit.)
    let _ = opts.bare_name;
    let main_name = format!("i18n_{index}");
    // `getTranslationConstPrefix(suffix)` = `('MSG_' + suffix).toUpperCase()`, then the unique index.
    let closure_name = format!(
        "{}{index}",
        format!("MSG_{}", opts.file_suffix).to_uppercase()
    );

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

    let mut initializers = vec![declare, if_stmt];

    // `i18n_const_collection.ts` `getTranslationDeclStmts`: when a placeholder's value merges more
    // than one position (the `[�…�|�…�]` form), the message string is postprocessed at runtime to
    // re-expand those merged placeholders. Angular appends, after the closure guard,
    // `i18n_<n> = ɵɵi18nPostprocess(i18n_<n>);`.
    if opts.needs_postprocess {
        let postprocess = o::import_expr(R3::I18nPostprocess.reference(), None)
            .call_fn(vec![main_var.clone()], false);
        initializers.push(Stmt::bare(StmtKind::Expression(
            main_var.clone().set(postprocess),
        )));
    }

    I18nConst {
        const_entry: main_var,
        initializers,
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

    // The `compute_msg_id_*` digest tests moved to `core::digest` alongside the
    // primitives (see split plan §3). The `message_decimal_digest_*` tests below
    // exercise i18n's `Message::decimal_digest()` which routes through the
    // re-exported `compute_msg_id`.

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
    fn format_value_tag_and_block_sentinels() {
        use I18nParamValueFlags as F;
        // Element open tag inside sub-template 1 at slot 1: `�#1:1�`.
        assert_eq!(
            format_value(&I18nParamValue {
                value: 1,
                sub_template_index: Some(1),
                flags: F::ELEMENT_TAG | F::OPEN_TAG,
            }),
            "\u{FFFD}#1:1\u{FFFD}"
        );
        // Element close tag: `�/#1:1�`.
        assert_eq!(
            format_value(&I18nParamValue {
                value: 1,
                sub_template_index: Some(1),
                flags: F::ELEMENT_TAG | F::CLOSE_TAG,
            }),
            "\u{FFFD}/#1:1\u{FFFD}"
        );
        // Template (block) open tag at slot 3, sub-template 1: `�*3:1�`.
        assert_eq!(
            format_value(&I18nParamValue {
                value: 3,
                sub_template_index: Some(1),
                flags: F::TEMPLATE_TAG | F::OPEN_TAG,
            }),
            "\u{FFFD}*3:1\u{FFFD}"
        );
        // Template close at slot 4, sub-template 2: `�/*4:2�`.
        assert_eq!(
            format_value(&I18nParamValue {
                value: 4,
                sub_template_index: Some(2),
                flags: F::TEMPLATE_TAG | F::CLOSE_TAG,
            }),
            "\u{FFFD}/*4:2\u{FFFD}"
        );
        // No flags ⇒ raw value (an expression index at the root): `0`.
        assert_eq!(
            format_value(&I18nParamValue {
                value: 0,
                sub_template_index: None,
                flags: F::NONE,
            }),
            "0"
        );
        // Self-closing (void) element ⇒ concatenated open+close: `�#1:1��/#1:1�`.
        assert_eq!(
            format_value(&I18nParamValue {
                value: 1,
                sub_template_index: Some(1),
                flags: F::ELEMENT_TAG | F::OPEN_TAG | F::CLOSE_TAG,
            }),
            "\u{FFFD}#1:1\u{FFFD}\u{FFFD}/#1:1\u{FFFD}"
        );
    }

    #[test]
    fn format_param_values_single_and_merged() {
        use I18nParamValueFlags as F;
        let a = I18nParamValue {
            value: 1,
            sub_template_index: Some(1),
            flags: F::ELEMENT_TAG | F::OPEN_TAG,
        };
        let b = I18nParamValue {
            value: 1,
            sub_template_index: Some(1),
            flags: F::ELEMENT_TAG | F::CLOSE_TAG,
        };
        assert_eq!(format_param_values(&[]), None);
        assert_eq!(format_param_values(&[a.clone()]), Some("\u{FFFD}#1:1\u{FFFD}".to_string()));
        assert_eq!(
            format_param_values(&[a, b]),
            Some("[\u{FFFD}#1:1\u{FFFD}|\u{FFFD}/#1:1\u{FFFD}]".to_string())
        );
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
