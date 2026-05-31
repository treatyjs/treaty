//! Static Ivy -> HTML interpreter: replays the deterministic instruction stream
//! Ivy lowers a template to (`ɵɵelement`/`ɵɵelementStart`/`ɵɵelementEnd` and the
//! `ɵɵdomElement*` variants + `ɵɵtext` in the create block, `ɵɵadvance` +
//! `ɵɵtextInterpolate*` in the update block) against the route's render data,
//! resolving `ctx.<path>` reads, HTML-escaping output, and serializing a
//! fragment. Ports the deterministic core of the TS `render.ts`.
//!
//! This is intentionally a focused interpreter, not a full Ivy VM: it covers the
//! static-content + text-interpolation subset that SSG prerender targets.
//! Instructions outside that subset (control flow, property/attribute bindings,
//! listeners, projection, higher-arity bindings whose args are not static consts
//! or `ctx` paths) are ignored rather than guessed at, so output is always a
//! faithful subset of the live render — never a wrong one — and the hydration
//! manifest tells the client runtime to take over the dynamic remainder.

use serde_json::Value;

use crate::types::{HydrationIsland, IslandKind, RenderData};

/// Void HTML elements that never get a closing tag.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

fn is_void_element(tag: &str) -> bool {
    let lower = tag.to_ascii_lowercase();
    VOID_ELEMENTS.contains(&lower.as_str())
}

/// One static attribute name/value pair accumulated from a create instruction's
/// consts array.
#[derive(Debug, Clone)]
struct Attr {
    name: String,
    value: String,
}

/// A node accumulated while replaying the Ivy create block. Children are stored
/// as arena indices (not inline nodes) so the update block can address a text
/// slot by stable index without fighting the borrow checker over a shared tree.
#[derive(Debug, Clone)]
enum RenderNode {
    /// An element with static attributes and arena-index children.
    Element { tag: String, attrs: Vec<Attr>, children: Vec<usize> },
    /// A text slot whose content the update block fills via interpolation, or a
    /// static literal from `ɵɵtext(i, "literal")`.
    Text { text: String },
}

/// A slot reference into the node arena: which arena entry, and (for elements)
/// the path of child indices to reach a child text node. The TS port mutates
/// nodes through shared references; in Rust the create-block tree is built in a
/// flat arena so the update block can address a slot's text by stable index.
///
/// Slots only ever point at top-level arena entries here: every create
/// instruction that takes a slot index records the arena index of the node it
/// produced, matching the TS `slots[slot] = node` behavior. Nesting is handled
/// by linking child arena indices onto their parent.
#[derive(Debug)]
struct Arena {
    nodes: Vec<RenderNode>,
    /// Roots (top-level arena indices, in source order).
    roots: Vec<usize>,
    /// Slot table: Ivy slot index -> arena index of the node placed there.
    slots: Vec<Option<usize>>,
    /// The open-element stack of arena indices (the current parent chain).
    stack: Vec<usize>,
}

impl Arena {
    fn new() -> Self {
        Self { nodes: Vec::new(), roots: Vec::new(), slots: Vec::new(), stack: Vec::new() }
    }

    /// Push a node into the arena and attach it to the current parent (or the
    /// root list when the stack is empty). Returns the new node's arena index.
    fn attach(&mut self, node: RenderNode) -> usize {
        let idx = self.nodes.len();
        self.nodes.push(node);
        match self.stack.last().copied() {
            Some(parent) => {
                if let RenderNode::Element { children, .. } = &mut self.nodes[parent] {
                    children.push(idx);
                }
            }
            None => self.roots.push(idx),
        }
        idx
    }

    /// Record the arena index for an Ivy slot, growing the slot table as needed.
    fn record_slot(&mut self, slot: usize, idx: usize) {
        if slot >= self.slots.len() {
            self.slots.resize(slot + 1, None);
        }
        self.slots[slot] = Some(idx);
    }
}

/// One parsed Ivy instruction: the bare name and its raw argument-list text.
#[derive(Debug)]
struct Instruction {
    name: String,
    args: String,
}

/// Escape a text node's content for safe HTML output. Mirrors the TS
/// `escapeText`: `&` then `<` then `>`.
fn escape_text(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Escape an attribute value for safe double-quoted output. Mirrors the TS
/// `escapeAttr`: `&` then `"`.
fn escape_attr(value: &str) -> String {
    value.replace('&', "&amp;").replace('"', "&quot;")
}

/// Stringify a render-data value the way a template interpolation would. Mirrors
/// the TS `stringifyBinding`: null/undefined -> "", string verbatim,
/// number/boolean via their display form, anything else as compact JSON.
fn stringify_binding(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Resolve a `ctx.a.b` member path against the render data. Mirrors the TS
/// `resolvePath`: walks object keys, returning `None` on any non-object step.
fn resolve_path<'a>(data: &'a RenderData, path: &[&str]) -> Option<&'a Value> {
    let mut keys = path.iter();
    let first = keys.next()?;
    let mut cur: &Value = data.get(*first)?;
    for key in keys {
        match cur {
            Value::Object(map) => cur = map.get(*key)?,
            _ => return None,
        }
    }
    Some(cur)
}

/// Isolate the body of the `*_Template(rf, ctx)` function from emitted Ivy JS.
/// Returns the source between the function's first `{` and its matching `}`, or
/// `None` when no template function is present. Mirrors the TS
/// `extractTemplateBody`.
fn extract_template_body(code: &str) -> Option<&str> {
    let (sig_start, sig_len) = find_template_sig(code)?;
    // `open` is the byte index of the function's opening `{`.
    let open = sig_start + sig_len - 1;
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&code[open + 1..i]);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Find the `function <id>_Template(...) {` signature, returning its byte start
/// and length (so the caller can locate the opening brace). Mirrors the TS
/// regex `/function\s+[A-Za-z_$][\w$]*_Template\s*\([^)]*\)\s*\{/`.
fn find_template_sig(code: &str) -> Option<(usize, usize)> {
    let bytes = code.as_bytes();
    let mut search = 0usize;
    while let Some(rel) = code[search..].find("function") {
        let fn_start = search + rel;
        if let Some(len) = match_template_sig_at(bytes, fn_start) {
            return Some((fn_start, len));
        }
        search = fn_start + "function".len();
    }
    None
}

/// Attempt to match the template-function signature starting at `function`.
/// Returns the total signature byte length (through the opening `{`) on success.
fn match_template_sig_at(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start + "function".len();
    // `\s+` — at least one whitespace.
    let ws = skip_ws(bytes, i);
    if ws == i {
        return None;
    }
    i = ws;
    // Identifier: `[A-Za-z_$][\w$]*`, captured to require a `_Template` suffix.
    let id_start = i;
    if !is_ident_start(bytes.get(i).copied()?) {
        return None;
    }
    i += 1;
    while i < bytes.len() && is_ident_part(bytes[i]) {
        i += 1;
    }
    let ident = &bytes[id_start..i];
    if !ident.ends_with(b"_Template") {
        return None;
    }
    // `\s*\(`.
    i = skip_ws(bytes, i);
    if bytes.get(i).copied()? != b'(' {
        return None;
    }
    i += 1;
    // `[^)]*\)`.
    while i < bytes.len() && bytes[i] != b')' {
        i += 1;
    }
    if bytes.get(i).copied()? != b')' {
        return None;
    }
    i += 1;
    // `\s*\{`.
    i = skip_ws(bytes, i);
    if bytes.get(i).copied()? != b'{' {
        return None;
    }
    i += 1;
    Some(i - start)
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$'
}

fn is_ident_part(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Tokenize the Ivy instruction calls (`iN.ɵɵfoo(args)` or `ɵɵfoo(args)`) in a
/// block of template-function source, in source order. Argument text is captured
/// raw (balanced parens). Mirrors the TS `parseInstructions` regex
/// `/(?:[A-Za-z_$][\w$]*\.)?(ɵɵ[A-Za-z]+\d*)\s*\(/g`.
///
/// The instruction name accepts a trailing digit run so the arity-suffixed
/// interpolation forms (`ɵɵtextInterpolate1`..`8`) tokenize as distinct names —
/// matching what [`apply_update_block`] / [`interpolate`] branch on and what
/// [`detect_islands`] counts (`ɵɵtextInterpolate\d*`). The TS regex's bare
/// `[A-Za-z]+` could not reach those forms; the digit run resolves that latent
/// gap without changing the letters-only create instructions.
fn parse_instructions(block: &str) -> Vec<Instruction> {
    let mut out = Vec::new();
    let chars: Vec<char> = block.chars().collect();
    let n = chars.len();
    let mut i = 0usize;
    while i < n {
        // Match the `ɵɵ` instruction-name marker.
        if chars[i] == 'ɵ' && i + 1 < n && chars[i + 1] == 'ɵ' {
            let name_start = i;
            let mut j = i + 2;
            while j < n && chars[j].is_ascii_alphabetic() {
                j += 1;
            }
            // Need at least one alphabetic char after `ɵɵ` to be a name.
            if j == i + 2 {
                i += 1;
                continue;
            }
            // Trailing digit run for the arity-suffixed interpolation forms.
            while j < n && chars[j].is_ascii_digit() {
                j += 1;
            }
            // `\s*\(`.
            let mut k = j;
            while k < n && chars[k].is_ascii_whitespace() {
                k += 1;
            }
            if k < n && chars[k] == '(' {
                let name: String = chars[name_start..j].iter().collect();
                let args_start = k + 1;
                // Capture balanced-paren argument text.
                let mut depth = 1i32;
                let mut p = args_start;
                while p < n && depth > 0 {
                    match chars[p] {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        _ => {}
                    }
                    p += 1;
                }
                // `p` is one past the closing `)`; args are [args_start, p-1).
                let end = if p > args_start { p - 1 } else { args_start };
                let args: String = chars[args_start..end].iter().collect();
                out.push(Instruction { name, args });
                i = p;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Split an instruction's raw argument text on top-level commas (ignoring commas
/// inside nested parens/brackets/braces or string literals). Empty parts are
/// dropped. Mirrors the TS `splitArgs`.
fn split_args(args: &str) -> Vec<String> {
    let chars: Vec<char> = args.chars().collect();
    let n = chars.len();
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut start = 0usize;
    let mut i = 0usize;
    while i < n {
        let ch = chars[i];
        if let Some(q) = quote {
            if ch == q && (i == 0 || chars[i - 1] != '\\') {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            '"' | '\'' | '`' => quote = Some(ch),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                let part: String = chars[start..i].iter().collect();
                parts.push(part.trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let tail: String = chars[start..].iter().collect();
    let tail = tail.trim().to_string();
    if !tail.is_empty() || !parts.is_empty() {
        parts.push(tail);
    }
    parts.into_iter().filter(|p| !p.is_empty()).collect()
}

/// Unquote a string-literal argument (`"x"`, `'x'`, `` `x` ``); returns `None`
/// otherwise. Mirrors the TS `asStringLiteral`.
fn as_string_literal(arg: &str) -> Option<String> {
    let t = arg.trim();
    let chars: Vec<char> = t.chars().collect();
    if chars.len() >= 2 {
        let first = chars[0];
        let last = chars[chars.len() - 1];
        if (first == '"' || first == '\'' || first == '`') && last == first {
            let inner: String = chars[1..chars.len() - 1].iter().collect();
            return Some(inner);
        }
    }
    None
}

/// Parse a `ctx.a.b` reference to its member path, or `None` for other exprs.
/// Mirrors the TS `asCtxPath` regex
/// `/^ctx\.((?:[A-Za-z_$][\w$]*)(?:\.[A-Za-z_$][\w$]*)*)$/`.
fn as_ctx_path(arg: &str) -> Option<Vec<String>> {
    let t = arg.trim();
    let rest = t.strip_prefix("ctx.")?;
    if rest.is_empty() {
        return None;
    }
    let mut path = Vec::new();
    for segment in rest.split('.') {
        let bytes = segment.as_bytes();
        if bytes.is_empty() || !is_ident_start(bytes[0]) || !bytes.iter().all(|&b| is_ident_part(b))
        {
            return None;
        }
        path.push(segment.to_string());
    }
    Some(path)
}

/// Parse a `ɵɵelement` consts attribute array (`["id", "main"]`) into name/value
/// pairs. Returns `[]` for the common no-attrs case (a numeric const index or
/// absent argument). Mirrors the TS `parseAttrs`.
fn parse_attrs(arg: Option<&String>) -> Vec<Attr> {
    let t = match arg {
        Some(a) => a.trim(),
        None => return Vec::new(),
    };
    if !t.starts_with('[') {
        return Vec::new();
    }
    // Strip the surrounding brackets, like TS `t.slice(1, -1)`.
    let chars: Vec<char> = t.chars().collect();
    let inner: String = if chars.len() >= 2 {
        chars[1..chars.len() - 1].iter().collect()
    } else {
        String::new()
    };
    let items: Vec<String> =
        split_args(&inner).iter().filter_map(|it| as_string_literal(it)).collect();
    let mut attrs = Vec::new();
    let mut i = 0usize;
    while i + 1 < items.len() {
        attrs.push(Attr { name: items[i].clone(), value: items[i + 1].clone() });
        i += 2;
    }
    attrs
}

/// Replay the create block to build the node arena. The create instructions form
/// a flat, slot-indexed, depth-first description of the DOM; `*Start`/`*End`
/// pairs push/pop the current parent, single-shot element/text instructions add
/// a leaf. Slot indices are recorded so the update block can address text nodes.
/// Mirrors the TS `buildCreateTree`.
fn build_create_tree(instructions: &[Instruction]) -> Arena {
    let mut arena = Arena::new();
    for ins in instructions {
        match ins.name.as_str() {
            "ɵɵelementStart" | "ɵɵdomElementStart" => {
                let parts = split_args(&ins.args);
                let slot = parse_slot(parts.first());
                let tag =
                    parts.get(1).and_then(|p| as_string_literal(p)).unwrap_or_else(|| "div".into());
                let attrs = parse_attrs(parts.get(2));
                let idx = arena.attach(RenderNode::Element { tag, attrs, children: Vec::new() });
                arena.stack.push(idx);
                if let Some(s) = slot {
                    arena.record_slot(s, idx);
                }
            }
            "ɵɵelementEnd" | "ɵɵdomElementEnd" => {
                arena.stack.pop();
            }
            "ɵɵelement" | "ɵɵdomElement" => {
                let parts = split_args(&ins.args);
                let slot = parse_slot(parts.first());
                let tag =
                    parts.get(1).and_then(|p| as_string_literal(p)).unwrap_or_else(|| "div".into());
                let attrs = parse_attrs(parts.get(2));
                let idx = arena.attach(RenderNode::Element { tag, attrs, children: Vec::new() });
                if let Some(s) = slot {
                    arena.record_slot(s, idx);
                }
            }
            "ɵɵtext" => {
                let parts = split_args(&ins.args);
                let slot = parse_slot(parts.first());
                let literal = if parts.len() > 1 {
                    parts.get(1).and_then(|p| as_string_literal(p)).unwrap_or_default()
                } else {
                    String::new()
                };
                let idx = arena.attach(RenderNode::Text { text: literal });
                if let Some(s) = slot {
                    arena.record_slot(s, idx);
                }
            }
            // Other create instructions (listeners, projection, …) carry no
            // static HTML we can faithfully emit; skip rather than guess.
            _ => {}
        }
    }
    arena
}

/// Parse the leading slot-index argument. Mirrors the TS `Number(parts[0])` +
/// `Number.isInteger(slot)` guard: only a clean non-negative integer is a slot.
fn parse_slot(arg: Option<&String>) -> Option<usize> {
    arg.and_then(|a| a.trim().parse::<usize>().ok())
}

/// Replay the update block, filling text slots from interpolation instructions.
/// `ɵɵadvance(n)` moves a virtual cursor across the slot table; the various
/// `ɵɵtextInterpolate*` instructions write the interpolated string into the slot
/// at the cursor. Mirrors the TS `applyUpdateBlock`.
fn apply_update_block(instructions: &[Instruction], arena: &mut Arena, data: &RenderData) {
    let mut cursor: usize = 0;
    for ins in instructions {
        match ins.name.as_str() {
            "ɵɵadvance" => {
                let parts = split_args(&ins.args);
                // `ɵɵadvance()` defaults to 1; a non-integer arg also falls back to 1.
                let by = match parts.first() {
                    Some(p) => p.trim().parse::<usize>().unwrap_or(1),
                    None => 1,
                };
                cursor += by;
            }
            name if name.starts_with("ɵɵtextInterpolate") => {
                let value = interpolate(name, &split_args(&ins.args), data);
                write_text(arena, cursor, value);
            }
            // Property bindings and other update instructions: leave the slot for
            // hydration to fill on the client.
            _ => {}
        }
    }
}

/// Write interpolated text into the slot at `cursor`, if it addresses a text
/// node. Mirrors the TS `writeText` (an element slot is left untouched).
fn write_text(arena: &mut Arena, cursor: usize, value: String) {
    if let Some(Some(idx)) = arena.slots.get(cursor).copied()
        && let Some(RenderNode::Text { text }) = arena.nodes.get_mut(idx)
    {
        *text = value;
    }
}

/// Compute the interpolated string for a `ɵɵtextInterpolate*` instruction.
/// `ɵɵtextInterpolate(expr)` is the single-binding form; the `N`-suffixed forms
/// interleave string literals and bindings. Mirrors the TS `interpolate`.
fn interpolate(name: &str, args: &[String], data: &RenderData) -> String {
    if name == "ɵɵtextInterpolate" {
        return eval_arg(args.first().map(String::as_str).unwrap_or(""), data);
    }
    // Interleaved form: a literal contributes verbatim, otherwise evaluate the arg.
    let mut out = String::new();
    for arg in args {
        match as_string_literal(arg) {
            Some(lit) => out.push_str(&lit),
            None => out.push_str(&eval_arg(arg, data)),
        }
    }
    out
}

/// Evaluate one interpolation argument to its string contribution. Mirrors the
/// TS `evalArg`: a quoted literal is verbatim, a `ctx.path` resolves against
/// `data`, anything else contributes the empty string.
fn eval_arg(arg: &str, data: &RenderData) -> String {
    if let Some(lit) = as_string_literal(arg) {
        return lit;
    }
    if let Some(path) = as_ctx_path(arg) {
        let refs: Vec<&str> = path.iter().map(String::as_str).collect();
        return stringify_binding(resolve_path(data, &refs));
    }
    String::new()
}

/// Serialize the built node arena (from its roots) to an HTML string. Mirrors
/// the TS `serialize`.
fn serialize(arena: &Arena) -> String {
    let mut html = String::new();
    serialize_nodes(arena, &arena.roots, &mut html);
    html
}

fn serialize_nodes(arena: &Arena, indices: &[usize], html: &mut String) {
    for &idx in indices {
        match &arena.nodes[idx] {
            RenderNode::Text { text } => html.push_str(&escape_text(text)),
            RenderNode::Element { tag, attrs, children } => {
                let mut attr_str = String::new();
                for a in attrs {
                    attr_str.push(' ');
                    attr_str.push_str(&a.name);
                    attr_str.push_str("=\"");
                    attr_str.push_str(&escape_attr(&a.value));
                    attr_str.push('"');
                }
                if is_void_element(tag) {
                    html.push('<');
                    html.push_str(tag);
                    html.push_str(&attr_str);
                    html.push('>');
                } else {
                    html.push('<');
                    html.push_str(tag);
                    html.push_str(&attr_str);
                    html.push('>');
                    serialize_nodes(arena, children, html);
                    html.push_str("</");
                    html.push_str(tag);
                    html.push('>');
                }
            }
        }
    }
}

/// Render the emitted Ivy JS for a component to a static HTML fragment, binding
/// interpolations against `data`. Returns the empty string when `code` carries
/// no recognizable template function (a pass-through module), so callers can
/// treat "nothing to prerender" uniformly. Mirrors the TS `renderIvyToHtml`.
///
/// The create block is everything the compiler emits under `rf & 1`; the update
/// block under `rf & 2`. Splitting on the `rf & 2` guard keeps the two
/// instruction streams apart so cursor/slot semantics match Ivy's.
pub fn render_ivy_to_html(code: &str, data: &RenderData) -> String {
    let body = match extract_template_body(code) {
        Some(b) => b,
        None => return String::new(),
    };

    let (create_src, update_src) = match find_update_guard(body) {
        Some(at) => (&body[..at], &body[at..]),
        None => (body, ""),
    };

    let mut arena = build_create_tree(&parse_instructions(create_src));
    if !update_src.is_empty() {
        apply_update_block(&parse_instructions(update_src), &mut arena, data);
    }
    serialize(&arena)
}

/// Locate the `if (rf & 2)` update-block guard, returning its byte offset within
/// `body`. Mirrors the TS regex `/if\s*\(\s*rf\s*&\s*2\s*\)/`.
fn find_update_guard(body: &str) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut search = 0usize;
    while let Some(rel) = body[search..].find("if") {
        let at = search + rel;
        if matches_update_guard(bytes, at) {
            return Some(at);
        }
        search = at + 2;
    }
    None
}

/// Match `if\s*\(\s*rf\s*&\s*2\s*\)` starting at the `if` keyword.
fn matches_update_guard(bytes: &[u8], start: usize) -> bool {
    let mut i = start + 2; // past `if`
    i = skip_ws(bytes, i);
    if bytes.get(i).copied() != Some(b'(') {
        return false;
    }
    i = skip_ws(bytes, i + 1);
    if !bytes[i..].starts_with(b"rf") {
        return false;
    }
    i = skip_ws(bytes, i + 2);
    if bytes.get(i).copied() != Some(b'&') {
        return false;
    }
    i = skip_ws(bytes, i + 1);
    if bytes.get(i).copied() != Some(b'2') {
        return false;
    }
    i = skip_ws(bytes, i + 1);
    bytes.get(i).copied() == Some(b')')
}

/// Count and describe the hydration islands in emitted Ivy `code`: the route
/// root `component` island plus one `interpolation` island per
/// `ɵɵtextInterpolate*` call the renderer filled. Mirrors the TS `detectIslands`
/// (regex `/ɵɵtextInterpolate\d*\s*\(/g`).
pub fn detect_islands(component_id: &str, code: &str) -> Vec<HydrationIsland> {
    let mut islands =
        vec![HydrationIsland { kind: IslandKind::Component, id: component_id.to_string() }];
    let count = count_interpolations(code);
    for i in 0..count {
        islands.push(HydrationIsland {
            kind: IslandKind::Interpolation,
            id: format!("{component_id}#{i}"),
        });
    }
    islands
}

/// Count `ɵɵtextInterpolate\d*\s*\(` occurrences in `code`.
fn count_interpolations(code: &str) -> usize {
    let chars: Vec<char> = code.chars().collect();
    let n = chars.len();
    let marker: Vec<char> = "ɵɵtextInterpolate".chars().collect();
    let mlen = marker.len();
    let mut count = 0usize;
    let mut i = 0usize;
    while i + mlen <= n {
        if chars[i..i + mlen] == marker[..] {
            let mut j = i + mlen;
            // `\d*`.
            while j < n && chars[j].is_ascii_digit() {
                j += 1;
            }
            // `\s*`.
            while j < n && chars[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < n && chars[j] == '(' {
                count += 1;
                i = j + 1;
                continue;
            }
        }
        i += 1;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn data(pairs: &[(&str, Value)]) -> RenderData {
        let mut m: RenderData = BTreeMap::new();
        for (k, v) in pairs {
            m.insert((*k).to_string(), v.clone());
        }
        m
    }

    /// A tiny `ɵɵelement` + `ɵɵtext` + `textInterpolate` template binding a
    /// `ctx.title` path, with HTML-significant characters in the data to prove
    /// escaping.
    #[test]
    fn element_text_interpolate_against_data() {
        let code = r#"
            function App_Template(rf, ctx) {
                if (rf & 1) {
                    i0.ɵɵelementStart(0, "h1");
                    i0.ɵɵtext(1);
                    i0.ɵɵelementEnd();
                }
                if (rf & 2) {
                    i0.ɵɵadvance(1);
                    i0.ɵɵtextInterpolate(ctx.title);
                }
            }
        "#;
        let d = data(&[("title", json!("Tom & <Jerry>"))]);
        let html = render_ivy_to_html(code, &d);
        assert_eq!(html, "<h1>Tom &amp; &lt;Jerry&gt;</h1>");
    }

    #[test]
    fn static_text_literal_is_emitted_and_escaped() {
        let code = r#"
            function Page_Template(rf, ctx) {
                if (rf & 1) {
                    ɵɵelementStart(0, "p");
                    ɵɵtext(1, "a < b & c");
                    ɵɵelementEnd();
                }
            }
        "#;
        let html = render_ivy_to_html(code, &BTreeMap::new());
        assert_eq!(html, "<p>a &lt; b &amp; c</p>");
    }

    #[test]
    fn dom_element_self_closing_with_attrs() {
        // ɵɵdomElement (single-shot) with a consts attribute array, plus a void
        // element that must not get a closing tag.
        let code = r#"
            function Hero_Template(rf, ctx) {
                if (rf & 1) {
                    ɵɵelementStart(0, "section", ["class", "hero"]);
                    ɵɵdomElement(1, "img", ["src", "/a.png", "alt", "A & B"]);
                    ɵɵelementEnd();
                }
            }
        "#;
        let html = render_ivy_to_html(code, &BTreeMap::new());
        assert_eq!(
            html,
            r#"<section class="hero"><img src="/a.png" alt="A &amp; B"></section>"#
        );
    }

    #[test]
    fn advance_addresses_correct_text_slot() {
        // Two text slots; advance must land the cursor on slot 3 for the second
        // interpolation while slot 1 stays the literal.
        let code = r#"
            function Two_Template(rf, ctx) {
                if (rf & 1) {
                    ɵɵelementStart(0, "div");
                    ɵɵtext(1, "static");
                    ɵɵelementStart(2, "span");
                    ɵɵtext(3);
                    ɵɵelementEnd();
                    ɵɵelementEnd();
                }
                if (rf & 2) {
                    ɵɵadvance(3);
                    ɵɵtextInterpolate(ctx.name);
                }
            }
        "#;
        let d = data(&[("name", json!("World"))]);
        let html = render_ivy_to_html(code, &d);
        assert_eq!(html, "<div>static<span>World</span></div>");
    }

    #[test]
    fn interpolate1_interleaves_literals_and_binding() {
        let code = r#"
            function Greet_Template(rf, ctx) {
                if (rf & 1) {
                    ɵɵelementStart(0, "p");
                    ɵɵtext(1);
                    ɵɵelementEnd();
                }
                if (rf & 2) {
                    ɵɵadvance(1);
                    ɵɵtextInterpolate1("Hello, ", ctx.name, "!");
                }
            }
        "#;
        let d = data(&[("name", json!("Ada"))]);
        let html = render_ivy_to_html(code, &d);
        assert_eq!(html, "<p>Hello, Ada!</p>");
    }

    #[test]
    fn nested_ctx_path_and_number_binding() {
        let code = r#"
            function Profile_Template(rf, ctx) {
                if (rf & 1) {
                    ɵɵelementStart(0, "span");
                    ɵɵtext(1);
                    ɵɵelementEnd();
                }
                if (rf & 2) {
                    ɵɵadvance(1);
                    ɵɵtextInterpolate(ctx.user.age);
                }
            }
        "#;
        let d = data(&[("user", json!({ "age": 42 }))]);
        let html = render_ivy_to_html(code, &d);
        assert_eq!(html, "<span>42</span>");
    }

    #[test]
    fn missing_ctx_path_renders_empty() {
        let code = r#"
            function X_Template(rf, ctx) {
                if (rf & 1) { ɵɵelementStart(0, "i"); ɵɵtext(1); ɵɵelementEnd(); }
                if (rf & 2) { ɵɵadvance(1); ɵɵtextInterpolate(ctx.nope); }
            }
        "#;
        let html = render_ivy_to_html(code, &BTreeMap::new());
        assert_eq!(html, "<i></i>");
    }

    #[test]
    fn no_template_function_returns_empty() {
        let code = "export const x = 1; function helper() { return 2; }";
        assert_eq!(render_ivy_to_html(code, &BTreeMap::new()), "");
    }

    #[test]
    fn detect_islands_counts_component_plus_interpolations() {
        let code = r#"
            function C_Template(rf, ctx) {
                if (rf & 2) {
                    ɵɵtextInterpolate(ctx.a);
                    ɵɵtextInterpolate1("x", ctx.b, "y");
                }
            }
        "#;
        let islands = detect_islands("page.tsx", code);
        assert_eq!(islands.len(), 3);
        assert_eq!(islands[0].kind, IslandKind::Component);
        assert_eq!(islands[0].id, "page.tsx");
        assert_eq!(islands[1].kind, IslandKind::Interpolation);
        assert_eq!(islands[1].id, "page.tsx#0");
        assert_eq!(islands[2].id, "page.tsx#1");
    }
}
