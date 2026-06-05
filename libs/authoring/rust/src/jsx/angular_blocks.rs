//! Angular control-flow blocks written *directly* inside JSX (`@if`/`@for`/`@switch`).
//!
//! OXC's JSX grammar cannot parse an Angular control-flow block written in child position: the
//! `{` after `@if (cond)` opens a JSX *expression container*, and the only body shape that
//! happens to parse is a single JSX element (`@if (c) { <p/> }`); a multi-element body
//! (`{ <p/> <p/> }`), a text/interpolation body (`{ hi {x} }`), or a nested block (`@switch`'s
//! `@case` chain) makes the whole TSX parse fail. We therefore handle these blocks *before* OXC
//! ever sees them, with a small source-level scanner.
//!
//! Strategy — a placeholder rewrite:
//!   1. [`preprocess`] scans the raw component source for an Angular control-flow block in JSX
//!      position (an `@if`/`@for`/`@switch` keyword followed by its head/body braces), parses the
//!      whole block chain (`@else`/`@else if`, `@empty`, `@case`/`@default`), lowers it to Angular
//!      template HTML (reusing the JSX body lowering by fragment-wrapping each body), and replaces
//!      the block's source text with a self-closing placeholder element `<treaty-cf-N />`.
//!   2. The placeholder is ordinary JSX, so OXC parses the rewritten source cleanly and the
//!      template visitor passes the placeholder element through verbatim.
//!   3. After lowering, [`restore`] swaps each `<treaty-cf-N />` placeholder in the template HTML
//!      back to its stashed Angular block, so the final template carries faithful `@if`/`@for`/
//!      `@switch` output — exactly what the `.treaty`/`.ts` path produces and what render3 parses.
//!
//! String/template-literal/comment context is tracked while scanning so an `@if` inside a string or
//! a `// @if` comment is never mistaken for a block. Decorator names (`@Component`) never collide:
//! we trigger only on the reserved control-flow keywords, which are never decorator identifiers.

use oxc_allocator::Allocator;
use oxc_ast::ast::{Expression, Statement};
use oxc_parser::Parser as JsParser;
use oxc_span::SourceType;

/// The placeholder tag emitted for a lowered control-flow block; `<treaty-cf-N />` carries the
/// block's slot index `N`. The name is deliberately not a real DOM/component tag so it round-trips
/// untouched through JSX lowering and is unambiguous to [`restore`].
const PLACEHOLDER_PREFIX: &str = "treaty-cf-";

/// The result of preprocessing a component source for Angular control-flow blocks: the rewritten
/// source (blocks replaced by placeholders) and the lowered Angular HTML for each placeholder slot.
pub(crate) struct Preprocessed {
    /// Source with every JSX-position control-flow block replaced by a `<treaty-cf-N />` placeholder.
    pub source: String,
    /// `blocks[N]` is the lowered Angular template HTML for placeholder `<treaty-cf-N />`.
    pub blocks: Vec<String>,
}

/// Scan `source` and replace each Angular control-flow block (in JSX position) with a placeholder
/// element, returning the rewritten source and the lowered Angular HTML keyed by placeholder index.
///
/// When the source contains no recognizable control-flow block, the source is returned unchanged
/// with an empty block list (the common case pays only one linear scan).
pub(crate) fn preprocess(source: &str) -> Preprocessed {
    let mut blocks: Vec<String> = Vec::new();
    let out = scan_and_rewrite(source, &mut blocks);
    Preprocessed { source: out, blocks }
}

/// Replace every `<treaty-cf-N />` placeholder in `template_html` with `blocks[N]`. The placeholder
/// is emitted by JSX lowering as `<treaty-cf-N />` (self-closing); we also accept the open/close
/// pair form defensively. Called on the lowered template before the signals pass so block-body
/// interpolations are auto-called consistently with the rest of the template.
pub(crate) fn restore(template_html: &str, blocks: &[String]) -> String {
    if blocks.is_empty() {
        return template_html.to_string();
    }
    let mut out = template_html.to_string();
    for (idx, block) in blocks.iter().enumerate() {
        let self_closing = format!("<{PLACEHOLDER_PREFIX}{idx} />");
        let open_close = format!("<{PLACEHOLDER_PREFIX}{idx}></{PLACEHOLDER_PREFIX}{idx}>");
        out = out.replace(&self_closing, block);
        out = out.replace(&open_close, block);
    }
    out
}

// ---------------------------------------------------------------------------
// Source scan.
// ---------------------------------------------------------------------------

/// The control-flow keywords that *open* a top-level block chain. `@else`/`@empty`/`@case`/
/// `@default` only appear as continuations *inside* a chain we already parse, so they never open a
/// chain on their own.
const OPENING_KEYWORDS: &[&str] = &["if", "for", "switch"];

/// Walk `source`, copying it to the output and, whenever an opening control-flow block is found in
/// a position where `@` is significant (not inside a string/template/comment), parse the whole
/// block chain, lower it, push the lowered HTML into `blocks`, and emit a `<treaty-cf-N />`
/// placeholder in its place.
fn scan_and_rewrite(source: &str, blocks: &mut Vec<String>) -> String {
    let bytes = source.as_bytes();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            // Skip string / template literals verbatim so an `@if` inside text is never matched.
            b'"' | b'\'' | b'`' => {
                let end = skip_string(bytes, i);
                out.push_str(&source[i..end]);
                i = end;
            }
            // Skip `//` line and `/* */` block comments verbatim.
            b'/' if i + 1 < bytes.len() && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') => {
                let end = skip_comment(bytes, i);
                out.push_str(&source[i..end]);
                i = end;
            }
            b'@' => {
                if let Some(block_end) = match_opening_block(source, i) {
                    // A full block chain spans `source[i..block_end]`; lower it and emit a placeholder.
                    let lowered = lower_block_chain(&source[i..block_end]);
                    let idx = blocks.len();
                    blocks.push(lowered);
                    out.push_str(&format!("<{PLACEHOLDER_PREFIX}{idx} />"));
                    i = block_end;
                } else {
                    out.push('@');
                    i += 1;
                }
            }
            _ => {
                // Advance one UTF-8 char so we never split a multibyte sequence.
                let ch_len = utf8_len(c);
                out.push_str(&source[i..i + ch_len]);
                i += ch_len;
            }
        }
    }
    out
}

/// If `source[at..]` begins a control-flow block (`@if`/`@for`/`@switch` + `(head)` + `{body}`),
/// return the byte index just past the whole block chain (including any `@else`/`@empty`/`@case`
/// continuations). Returns `None` when the `@` is not an opening control-flow keyword or the block
/// is not well-formed.
fn match_opening_block(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    let kw_start = at + 1;
    let kw_end = ident_end(bytes, kw_start);
    let keyword = &source[kw_start..kw_end];
    if !OPENING_KEYWORDS.contains(&keyword) {
        return None;
    }

    let mut cursor = skip_ws(bytes, kw_end);
    // An opening control-flow keyword is always followed by a parenthesized head (`@if (cond)`,
    // `@for (x of xs; track x)`, `@switch (v)`); without it this is not a block we own.
    if cursor >= bytes.len() || bytes[cursor] != b'(' {
        return None;
    }
    cursor = match_balanced(bytes, cursor, b'(', b')')?;
    cursor = skip_ws(bytes, cursor);
    if cursor >= bytes.len() || bytes[cursor] != b'{' {
        return None;
    }
    cursor = match_balanced(bytes, cursor, b'{', b'}')?;

    // Consume continuation blocks that belong to this chain: `@else`/`@else if`/`@empty`/`@case`/
    // `@default`. Each is `@kw [ (head) ] { body }`. Whitespace between continuations is skipped.
    loop {
        let after_ws = skip_ws(bytes, cursor);
        let Some(end) = match_continuation(source, after_ws) else {
            break;
        };
        cursor = end;
    }
    Some(cursor)
}

/// Match a continuation block at `at` (`@else`/`@empty`/`@case`/`@default`, each with an optional
/// parenthesized head and a brace body), returning the index past it, or `None` if there is none.
fn match_continuation(source: &str, at: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if at >= bytes.len() || bytes[at] != b'@' {
        return None;
    }
    let kw_start = at + 1;
    let kw_end = ident_end(bytes, kw_start);
    let keyword = &source[kw_start..kw_end];
    if !matches!(keyword, "else" | "empty" | "case" | "default") {
        return None;
    }

    let mut cursor = skip_ws(bytes, kw_end);
    // `@else if (cond)` — the `if` keyword sits between `@else` and the head.
    if keyword == "else" && bytes.get(cursor) == Some(&b'i') {
        let if_end = ident_end(bytes, cursor);
        if &source[cursor..if_end] == "if" {
            cursor = skip_ws(bytes, if_end);
        }
    }
    // Optional parenthesized head (`@else` and `@default` have none; `@else if`/`@case` do).
    if bytes.get(cursor) == Some(&b'(') {
        cursor = match_balanced(bytes, cursor, b'(', b')')?;
        cursor = skip_ws(bytes, cursor);
    }
    if bytes.get(cursor) != Some(&b'{') {
        return None;
    }
    match_balanced(bytes, cursor, b'{', b'}')
}

// ---------------------------------------------------------------------------
// Block lowering.
// ---------------------------------------------------------------------------

/// Lower a full control-flow block chain source (`@if (…) { … } @else { … }`, `@for (…) { … }
/// @empty { … }`, `@switch (…) { @case (…) { … } @default { … } }`) to Angular template HTML.
///
/// Each `@keyword (head) {` segment is re-emitted with its head verbatim, and each `{ body }` is
/// lowered by recursively preprocessing it (so nested blocks compose) then fragment-parsing it and
/// running the JSX body lowering — the same lowering ordinary JSX children get.
fn lower_block_chain(chain: &str) -> String {
    let bytes = chain.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        // Each segment starts at an `@keyword`. Copy the keyword + head verbatim, then lower body.
        if bytes[i] == b'@' {
            let kw_start = i + 1;
            let kw_end = ident_end(bytes, kw_start);
            let keyword = &chain[kw_start..kw_end];

            // Emit `@keyword`.
            if !out.is_empty() {
                out.push(' ');
            }
            out.push('@');
            out.push_str(keyword);

            let mut cursor = skip_ws(bytes, kw_end);
            // `@else if` — copy the `if` keyword through.
            if keyword == "else" && chain[cursor..].starts_with("if") {
                let if_end = ident_end(bytes, cursor);
                if &chain[cursor..if_end] == "if" {
                    out.push_str(" if");
                    cursor = skip_ws(bytes, if_end);
                }
            }
            // Optional parenthesized head — copy verbatim (the binding/track/condition expressions
            // are Angular template expressions and pass through unchanged).
            if bytes.get(cursor) == Some(&b'(') {
                let head_end = match_balanced(bytes, cursor, b'(', b')')
                    .unwrap_or(bytes.len());
                out.push(' ');
                out.push_str(chain[cursor..head_end].trim());
                cursor = skip_ws(bytes, head_end);
            }
            // Body `{ … }`. The lowered body is trimmed so the canonical Angular spelling
            // (`@kw (head) { body }`) carries exactly one space inside each brace, regardless of the
            // author's whitespace around the block body in the source.
            //
            // A `@switch` body is itself a chain of `@case`/`@default` blocks (not ordinary JSX), so
            // it is lowered by recursing through this same chain lowering; every other block's body
            // is ordinary JSX content lowered via [`lower_block_body`].
            out.push_str(" { ");
            if bytes.get(cursor) == Some(&b'{') {
                let body_end = match_balanced(bytes, cursor, b'{', b'}')
                    .unwrap_or(bytes.len());
                let body = &chain[cursor + 1..body_end - 1];
                let lowered_body = if keyword == "switch" {
                    lower_block_chain(body)
                } else {
                    lower_block_body(body)
                };
                out.push_str(lowered_body.trim());
                cursor = body_end;
            }
            out.push_str(" }");
            i = cursor;
        } else {
            i += 1;
        }
    }
    out
}

/// Lower the raw source of a single block body to Angular template HTML.
///
/// The body may itself contain nested control-flow blocks, so it is preprocessed first (replacing
/// nested blocks with placeholders), then fragment-wrapped and parsed with OXC (a fragment accepts
/// any sequence of elements/text/interpolation, which a bare expression container would reject),
/// lowered with the JSX fragment lowering, and finally the nested placeholders are restored.
fn lower_block_body(body: &str) -> String {
    let pre = preprocess(body);
    let wrapped = format!("const __cf = <>{}</>;", pre.source);
    let allocator = Allocator::default();
    let ret = JsParser::new(&allocator, &wrapped, SourceType::tsx()).parse();

    let lowered = ret
        .program
        .body
        .iter()
        .find_map(|stmt| {
            let Statement::VariableDeclaration(decl) = stmt else {
                return None;
            };
            decl.declarations.iter().find_map(|d| match &d.init {
                Some(Expression::JSXFragment(frag)) => {
                    Some(super::template::lower_fragment(frag, &wrapped))
                }
                Some(Expression::JSXElement(el)) => {
                    Some(super::template::lower_element(el, &wrapped))
                }
                _ => None,
            })
        })
        .unwrap_or_default();

    restore(&lowered, &pre.blocks)
}

// ---------------------------------------------------------------------------
// Byte-level scan helpers.
// ---------------------------------------------------------------------------

/// The number of bytes in the UTF-8 character whose lead byte is `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

/// Index just past the identifier starting at `start` (ASCII letters/digits/`_`/`$`).
fn ident_end(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' {
            i += 1;
        } else {
            break;
        }
    }
    i
}

/// Index of the first non-whitespace byte at or after `start`.
fn skip_ws(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Given `bytes[start]` is the opening delimiter `open`, return the index just past the matching
/// `close`, accounting for nesting and for strings/templates/comments inside the span. Returns
/// `None` if the delimiter is never balanced.
fn match_balanced(bytes: &[u8], start: usize, open: u8, close: u8) -> Option<usize> {
    debug_assert_eq!(bytes[start], open);
    let mut depth = 0usize;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            b'"' | b'\'' | b'`' => {
                i = skip_string(bytes, i);
                continue;
            }
            b'/' if i + 1 < bytes.len() && (bytes[i + 1] == b'/' || bytes[i + 1] == b'*') => {
                i = skip_comment(bytes, i);
                continue;
            }
            _ if c == open => {
                depth += 1;
                i += 1;
            }
            _ if c == close => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => i += 1,
        }
    }
    None
}

/// Given `bytes[start]` is a string/template quote, return the index just past the closing quote,
/// honouring backslash escapes. Template literals (`` ` ``) are treated as opaque text; a `${…}`
/// interpolation inside is scanned shallowly (its braces are skipped via [`match_balanced`]) so a
/// quote inside the interpolation does not prematurely end the literal.
fn skip_string(bytes: &[u8], start: usize) -> usize {
    let quote = bytes[start];
    let mut i = start + 1;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' {
            i += 2;
            continue;
        }
        if quote == b'`' && c == b'$' && bytes.get(i + 1) == Some(&b'{') {
            i = match_balanced(bytes, i + 1, b'{', b'}').unwrap_or(bytes.len());
            continue;
        }
        if c == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

/// Given `bytes[start..]` begins a `//` or `/* */` comment, return the index just past it.
fn skip_comment(bytes: &[u8], start: usize) -> usize {
    if bytes[start + 1] == b'/' {
        let mut i = start + 2;
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        i
    } else {
        let mut i = start + 2;
        while i + 1 < bytes.len() {
            if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                return i + 2;
            }
            i += 1;
        }
        bytes.len()
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Preprocess a component source, then restore placeholders directly (no signals pass), so the
    /// final Angular-block HTML can be asserted in isolation.
    fn lower(source: &str) -> String {
        let pre = preprocess(source);
        restore(&pre.source, &pre.blocks)
    }

    #[test]
    fn if_block_lowers() {
        let out = lower("<div>@if (show) { <p>hi</p> }</div>");
        assert_eq!(out, "<div><treaty-cf-0 /></div>".replace(
            "<treaty-cf-0 />",
            "@if (show) { <p>hi</p> }"
        ));
    }

    #[test]
    fn if_block_multi_child_body_lowers() {
        // The body that OXC cannot parse as a bare expression container (two sibling elements).
        let out = lower("<div>@if (show) { <p>a</p> <p>b</p> }</div>");
        assert_eq!(out, "<div>@if (show) { <p>a</p> <p>b</p> }</div>");
    }

    #[test]
    fn if_block_text_interpolation_body_lowers() {
        // A text + interpolation body (`hi {x}`) also fails OXC's bare-container parse; here it is
        // lowered to text + `{{ x }}`.
        let out = lower("<div>@if (show) { hi {x} }</div>");
        assert_eq!(out, "<div>@if (show) { hi{{ x }} }</div>");
    }

    #[test]
    fn if_else_block_lowers() {
        let out = lower("<div>@if (a) { <p>a</p> } @else { <p>b</p> }</div>");
        assert_eq!(out, "<div>@if (a) { <p>a</p> } @else { <p>b</p> }</div>");
    }

    #[test]
    fn if_else_if_else_chain_lowers() {
        let out = lower("<div>@if (a) { <p>a</p> } @else if (b) { <p>b</p> } @else { <p>c</p> }</div>");
        assert_eq!(
            out,
            "<div>@if (a) { <p>a</p> } @else if (b) { <p>b</p> } @else { <p>c</p> }</div>"
        );
    }

    #[test]
    fn for_block_lowers_with_track() {
        let out = lower("<ul>@for (x of xs; track x) { <li>{x}</li> }</ul>");
        assert_eq!(out, "<ul>@for (x of xs; track x) { <li>{{ x }}</li> }</ul>");
    }

    #[test]
    fn for_empty_block_lowers() {
        let out = lower("<ul>@for (x of xs; track x) { <li>{x}</li> } @empty { <li>none</li> }</ul>");
        assert_eq!(
            out,
            "<ul>@for (x of xs; track x) { <li>{{ x }}</li> } @empty { <li>none</li> }</ul>"
        );
    }

    #[test]
    fn switch_block_lowers() {
        // The block OXC outright rejects (nested `@case` chain inside the switch braces).
        let out = lower("<div>@switch (v) { @case (1) { <p>one</p> } @default { <p>z</p> } }</div>");
        assert_eq!(
            out,
            "<div>@switch (v) { @case (1) { <p>one</p> } @default { <p>z</p> } }</div>"
        );
    }

    #[test]
    fn nested_for_inside_if_lowers() {
        let out = lower("<div>@if (show) { @for (x of xs; track x) { <li>{x}</li> } }</div>");
        assert_eq!(
            out,
            "<div>@if (show) { @for (x of xs; track x) { <li>{{ x }}</li> } }</div>"
        );
    }

    #[test]
    fn at_sign_in_string_is_not_a_block() {
        // An `@if` inside a string literal must not be treated as a block.
        let out = lower("<a href=\"mailto:@if (x)\">x</a>");
        assert_eq!(out, "<a href=\"mailto:@if (x)\">x</a>");
    }

    #[test]
    fn decorator_keyword_is_not_a_control_flow_block() {
        // `@Component(...)` is a decorator, not a control-flow block: `Component` is not in the
        // opening keyword set, so it is left untouched.
        let out = lower("@Component({ selector: 'x' })\nclass X {}");
        assert_eq!(out, "@Component({ selector: 'x' })\nclass X {}");
    }

    #[test]
    fn source_without_blocks_is_unchanged() {
        let src = "export default function App() { return <div>hi</div>; }";
        let pre = preprocess(src);
        assert!(pre.blocks.is_empty());
        assert_eq!(pre.source, src);
    }
}
