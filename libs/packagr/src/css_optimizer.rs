//! CSS minification of scoped component styles toward **ng-packagr@21** — the
//! optimizer whose oracle is ng-packagr's emitted FESM `styles`, NOT esbuild.
//!
//! ## The oracle is ng-packagr's FESM, not esbuild
//!
//! ng-packagr's emitted byte string is the ground truth here, captured from a real
//! `ng-packagr@21.2.3 + @angular/core 22.0.0-rc.3` full build (see
//! `D:\tmp\css-oracle`). Earlier revisions of this module claimed byte-equality with
//! `esbuild.transformSync({ minify: true })`. That claim was WRONG for at least one
//! construct: ng-packagr's FESM emits `@media (min-width: 600px)` → `@media(min-width:600px)`
//! (TIGHT), whereas BOTH a plain `esbuild.transformSync(minify:true)` AND ng-packagr's
//! own exact esbuild `build` invocation (`bundle:true, minify:true, supported:{nesting:false}`,
//! browserslist target) AND `@angular/compiler.encapsulateStyle(esbuildBundleFile(css))`
//! all leave that prelude LOOSE (`@media (min-width: 600px)`). So the ng FESM applies a
//! TIGHTER `@media` prelude than any single intermediate stage we could probe — the FESM
//! bytes are the only authority, and this module targets them directly. Every expectation
//! below is annotated with the captured ng-packagr bytes it reproduces.
//!
//! ## ng-packagr's real pipeline vs treaty's pipeline
//!
//! ng-packagr order is `encapsulateStyle( esbuildBundleFile( css ) )`:
//!
//! ```text
//!   SCSS/CSS ─preprocess─▶ plain CSS ─[1] esbuild minify─▶ [2] ngc ShadowCss scope ─▶ FESM emit
//! ```
//!
//! esbuild (stage 1) runs BEFORE scoping and does the value/selector transforms on the
//! UNSCOPED CSS; ngc's `ShadowCss` (stage 2, = `encapsulateStyle`, the identical
//! algorithm treaty_ivy ports) then appends `[_ngcontent-%COMP%]`, re-spaces combinators
//! to ` > ` ` + ` ` ~ `, and re-joins selector lists with `, ` (comma+SPACE). ShadowCss is
//! whitespace-PRESERVING for at-rule preludes.
//!
//! treaty's order is the REVERSE — `optimize( ShadowCss( css ) )`:
//!
//! ```text
//!   SCSS/CSS ─preprocess─▶ plain CSS ─[1] treaty_ivy ShadowCss scope─▶ [2] THIS optimizer ─▶ FESM emit
//! ```
//!
//! treaty_ivy already scopes ([`crate::stylesheet`] resolves + preprocesses, treaty_ivy's
//! `ShadowCss` port scopes), and this optimizer then runs as a post-pass over each emitted
//! `styles: [...]` string (covering inline `styles` and resolved `styleUrls` uniformly).
//! Because treaty's esbuild-equivalent pass runs AFTER scoping rather than before, this
//! optimizer must reproduce exactly those esbuild transforms that SURVIVE ShadowCss, while
//! NOT re-doing what ShadowCss already normalized. Concretely:
//!
//!   * it must NOT tighten the selector-list `, ` back to `,` — ShadowCss legitimately
//!     re-joins selector lists with comma+SPACE and ng-packagr keeps it;
//!   * it must NOT re-space combinators — ShadowCss already emits ` > ` ` + ` ` ~ `;
//!   * it MUST apply the esbuild transforms that ran pre-scope on selectors/values and
//!     then survived ShadowCss: tighten the `@media` feature query, rewrite a keyframe
//!     `from` → `0%` and a zero-angle `rotate(0deg)` → `rotate(0)`, strip
//!     attribute-selector quotes on identifier values (`[data-state=open]`), collapse a
//!     legacy `::before` → `:before`, tighten `red !important` → `red!important`;
//!   * it MUST leave custom-property (`--x`) values completely verbatim — ng-packagr does
//!     NOT minify them (`--x: #336699`, full 6-digit hex and the space after the colon
//!     preserved), so they are excluded from value minification.
//!
//! ## Context-free value transforms (matching the ng-packagr bytes)
//!
//! Like esbuild, ng-packagr's FESM converts a color name to hex only in `<color>`-typed
//! positions and never re-serializes a color inside a shorthand. Reproducing that full
//! typed model is a CSS engine in its own right, so this optimizer implements the
//! transforms that are **context-free** (correct regardless of property) and is
//! conservative everywhere else:
//!
//!   * whitespace collapse + selector/decl punctuation tightening;
//!   * hex color normalization `#AABBCC` → `#abc`, `#FFFFFF` → `#fff`, 8→4 digit
//!     shortening, lowercasing (a hex token means the same in any position);
//!   * numeric normalization: drop a leading integer zero (`0.5`→`.5`), trailing
//!     fractional zeros (`0.50`→`.5`, `10.0`→`10`), and the unit on a zero *length*
//!     (`0px`→`0`, `0em`→`0`) while keeping a zero percentage/time (`0%`, `0s`);
//!   * `font-weight` keyword → number (`bold`→`700`, `normal`→`400`) — `font-weight` only;
//!   * color *name* → hex (`blue`→`#00f`) — only in a **color-only longhand** position
//!     (`color`, `background-color`, `border-color`, …) whose value is a single color
//!     token, the positions where ng-packagr/esbuild always converts;
//!   * transform-function shortening: single-axis `translateX(<v>)` → `translate(<v>)`,
//!     and a zero-angle `rotate(0deg)`/`skew(0deg)` → `rotate(0)`/`skew(0)`.
//!
//! Color names inside shorthands (`border`, `background`, `outline`, `box-shadow`) and
//! `rgb()`/`hsl()` function conversions are deliberately NOT performed — doing them
//! blindly would *over*-minify relative to the ng-packagr bytes. The honest result: the
//! value-level output is byte-equal to the ng-packagr@21 FESM for the common
//! single-color/whitespace/number/hex cases, and conservatively unchanged (never wrong)
//! for shorthand colors and functional color notations.

/// Optimize one already-scoped CSS string toward the **ng-packagr@21 FESM** `styles`
/// bytes (the oracle), assuming the input has already been scoped by treaty_ivy's
/// `ShadowCss` port (so selector lists arrive as `, ` and combinators as ` > `).
///
/// Preserves Angular's `_ngcontent-%COMP%` / `_nghost-%COMP%` scoping placeholders
/// (they live in selectors, which this pass only whitespace-tightens). Returns the
/// minified CSS WITHOUT a trailing newline (the caller decides framing); ng-packagr's
/// FESM `styles` literal likewise ends at the final `}` with no trailing newline.
pub fn optimize(css: &str) -> String {
    let tokens = tokenize(css);
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    // Track whether we are inside a declaration block (between `{` and `}`) so we
    // only treat `:` as a declaration separator there (selectors can carry `:` in
    // pseudo-classes, which must not be split into property/value).
    let mut depth: i32 = 0;
    // Per-block at-rule kind stack, pushed on `{` / popped on `}`. The selectors
    // directly inside a `@keyframes` block are keyframe selectors (`from`/`to`/`0%`),
    // where esbuild rewrites `from` → `0%`. The kind is captured from the most recent
    // at-rule prelude rendered before the `{`.
    let mut block_kinds: Vec<BlockKind> = Vec::new();
    let mut pending_kind = BlockKind::Other;
    while i < tokens.len() {
        match &tokens[i] {
            Tok::Punct('{') => {
                depth += 1;
                block_kinds.push(pending_kind);
                pending_kind = BlockKind::Other;
                out.push('{');
                i += 1;
            }
            Tok::Punct('}') => {
                depth -= 1;
                block_kinds.pop();
                // esbuild drops the last declaration's terminating semicolon before
                // a block close (`color:#00f;}` → `color:#00f}`).
                if out.ends_with(';') {
                    out.pop();
                }
                out.push('}');
                i += 1;
            }
            // An at-rule prelude (`@media …`, `@supports …`, `@keyframes …`). Consume
            // the whole prelude up to (not including) the `{` and render it with
            // at-rule-specific whitespace handling.
            Tok::Word(w) if w.starts_with('@') => {
                let (rendered, next, kind) = render_at_rule_prelude(&tokens, i);
                out.push_str(&rendered);
                pending_kind = kind;
                i = next;
            }
            // Insignificant whitespace in selector context: keep it as a single
            // space ONLY when it sits between two non-punctuation tokens (a
            // descendant combinator). Drop it adjacent to any structural
            // punctuation (`{ } ; , :` and around combinators).
            Tok::Space => {
                if space_is_significant(&tokens, i) {
                    out.push(' ');
                }
                i += 1;
            }
            // A declaration: at block depth, scan `<prop> : <value> ;` and rewrite
            // the value with property awareness.
            _ if depth > 0 && is_declaration_start(&tokens, i) => {
                let (decl, next) = take_declaration(&tokens, i);
                out.push_str(&decl);
                i = next;
            }
            // A legacy CSS2 pseudo-element written with the modern `::` syntax —
            // `::before`/`::after`/`::first-line`/`::first-letter` collapse to a single
            // colon (`:before`), exactly as esbuild serializes them. Detected as two
            // adjacent `:` punctuation tokens followed by a legacy pseudo name.
            Tok::Punct(':')
                if matches!(tokens.get(i + 1), Some(Tok::Punct(':')))
                    && matches!(tokens.get(i + 2), Some(Tok::Word(name)) if is_legacy_pseudo_element(name)) =>
            {
                let Some(Tok::Word(name)) = tokens.get(i + 2) else {
                    unreachable!()
                };
                out.push(':');
                out.push_str(name);
                i += 3;
            }
            // A quoted attribute-selector value whose value is a bare CSS identifier
            // (`[data-state="open"]`) has its quotes stripped (`[data-state=open]`),
            // matching esbuild. A value that is NOT a valid identifier (`"a b"`, `"1n"`)
            // keeps its quotes. Only applies in selector context, where the preceding
            // emitted text ends with `=` (inside `[attr=…]`).
            Tok::Str(s) if out.ends_with('=') && is_css_identifier(s) => {
                out.push_str(s);
                i += 1;
            }
            // A `from` keyframe selector inside a `@keyframes` block → `0%` (esbuild
            // normalizes the `from` keyword to the equivalent `0%`; `to` is left as-is,
            // since `to` is shorter than `100%`). Only fires directly inside a keyframes
            // block, never on a `from` that happens to appear elsewhere.
            Tok::Word(w)
                if w.eq_ignore_ascii_case("from")
                    && block_kinds.last() == Some(&BlockKind::Keyframes) =>
            {
                out.push_str("0%");
                i += 1;
            }
            _ => {
                // Selector / at-rule prelude text — emit tightened.
                out.push_str(&render_selector_token(&tokens[i]));
                i += 1;
            }
        }
    }
    out
}

/// The at-rule kind of an open `{ … }` block, tracked so keyframe selectors can be
/// rewritten (`from` → `0%`) only inside a `@keyframes` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Keyframes,
    Other,
}

/// Render an at-rule prelude starting at the `@`-keyword token `i`, consuming every
/// token up to (but not including) the opening `{`. Returns the rendered prelude text,
/// the index of the `{` (or end), and the [`BlockKind`] the prelude opens.
///
/// ## Whitespace, per the ng-packagr@21 oracle (captured FESM bytes, not asserted)
///
/// ng-packagr's emitted FESM tightens a `@media` feature query but leaves `@supports`
/// and other at-rule preludes loose:
///   * `@media (min-width: 600px)`  → `@media(min-width:600px)`  (TIGHT)
///   * `@media screen and (min-width: 600px)` → `@media screen and (min-width:600px)`
///     (the `screen and ` keywords keep their spaces; only the parenthesized feature
///     query is tightened, and the `@media`↔`(` space is dropped only when `(` follows
///     `@media` directly)
///   * `@supports (display: grid)`  → `@supports (display: grid)`  (LOOSE — untouched)
///   * `@keyframes name`            → `@keyframes name`            (LOOSE — name kept)
///
/// So only `@media`-family preludes are tightened; every other at-rule prelude is
/// rendered with its authored spacing preserved (descendant-style single spaces).
fn render_at_rule_prelude(tokens: &[Tok], i: usize) -> (String, usize, BlockKind) {
    let Tok::Word(at) = &tokens[i] else {
        return (render_selector_token(&tokens[i]), i + 1, BlockKind::Other);
    };
    let at_lower = at.to_ascii_lowercase();
    let kind = if at_lower.starts_with("@keyframes") || at_lower.starts_with("@-webkit-keyframes") {
        BlockKind::Keyframes
    } else {
        BlockKind::Other
    };
    let is_media = at_lower == "@media" || at_lower == "@-webkit-media";

    // Gather the prelude tokens (everything up to the `{`).
    let mut j = i;
    let mut prelude: Vec<&Tok> = Vec::new();
    while j < tokens.len() && !matches!(tokens[j], Tok::Punct('{')) {
        prelude.push(&tokens[j]);
        j += 1;
    }

    if !is_media {
        // Non-`@media` at-rule: render loose, preserving authored single spaces between
        // words (`@supports (display: grid)`, `@keyframes name`) and dropping spaces
        // adjacent to structural punctuation.
        let mut out = String::new();
        for (k, t) in prelude.iter().enumerate() {
            match t {
                Tok::Space => {
                    if prelude_space_is_significant(&prelude, k) {
                        out.push(' ');
                    }
                }
                other => out.push_str(&render_selector_token(other)),
            }
        }
        return (out, j, kind);
    }

    // `@media` prelude: tighten the parenthesized feature queries and drop the single
    // space between `@media` and a directly-following `(`.
    let mut out = String::new();
    for (k, t) in prelude.iter().enumerate() {
        match t {
            Tok::Space => {
                // Drop this space iff the next token is a paren group AND the previous
                // significant token is `@media` itself (`@media (min-width…)` → no space).
                let next_is_paren = matches!(
                    prelude.get(k + 1),
                    Some(Tok::Word(w)) if w.starts_with('(')
                );
                let prev_is_at_media = matches!(
                    prelude[..k].iter().rev().find(|t| !matches!(t, Tok::Space)),
                    Some(Tok::Word(w)) if w.eq_ignore_ascii_case("@media")
                );
                if !(next_is_paren && prev_is_at_media)
                    && prelude_space_is_significant(&prelude, k)
                {
                    out.push(' ');
                }
            }
            Tok::Word(w) if w.starts_with('(') => out.push_str(&tighten_media_feature(w)),
            other => out.push_str(&render_selector_token(other)),
        }
    }
    (out, j, kind)
}

/// Whether the whitespace at index `k` of an at-rule prelude slice is significant
/// (a single space between two words). Mirrors [`space_is_significant`] for the
/// prelude-local token slice.
fn prelude_space_is_significant(prelude: &[&Tok], k: usize) -> bool {
    let prev = prelude[..k].iter().rev().find(|t| !matches!(t, Tok::Space));
    let next = prelude[k + 1..].iter().find(|t| !matches!(t, Tok::Space));
    fn keeps(tok: Option<&&Tok>) -> bool {
        matches!(tok, Some(Tok::Word(_)) | Some(Tok::Str(_)))
    }
    keeps(prev) && keeps(next)
}

/// Tighten a captured `@media` feature-query group: drop the space after the colon
/// (`(min-width: 600px)` → `(min-width:600px)`) and any spaces adjacent to the parens,
/// matching the ng-packagr@21 FESM. Unlike [`tighten_parens`] (used for `calc()` /
/// `rgb()` value functions, which keep an interior space after a colon), the media
/// feature query is fully tightened around its `:`.
fn tighten_media_feature(group: &str) -> String {
    let mut out = String::with_capacity(group.len());
    let chars: Vec<char> = group.chars().collect();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c.is_whitespace() {
            let mut m = k;
            while m < chars.len() && chars[m].is_whitespace() {
                m += 1;
            }
            let prev = out.chars().last();
            let next = chars.get(m).copied();
            // Drop the space when adjacent to a paren, a comma, or a colon (the media
            // feature separator), else collapse to a single space (e.g. inside a
            // `calc()` nested in a range query).
            let drop = matches!(prev, Some('(') | Some(',') | Some(':'))
                || matches!(next, Some(')') | Some(',') | Some(':'));
            if !drop && prev.is_some() && next.is_some() {
                out.push(' ');
            }
            k = m;
            continue;
        }
        out.push(c);
        k += 1;
    }
    out
}

/// Whether `name` is one of the four legacy CSS2 pseudo-elements that may be written
/// with a single colon (`:before`, `:after`, `:first-line`, `:first-letter`) — the set
/// esbuild collapses from the modern `::` syntax back to `:`. Modern pseudo-elements
/// (`::selection`, `::placeholder`, `::backdrop`, …) keep their `::`.
fn is_legacy_pseudo_element(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "before" | "after" | "first-line" | "first-letter"
    )
}

/// Whether `s` is a valid CSS identifier (an `ident-token`): a first char that is a
/// letter, `-`, `_`, or non-ASCII, followed by letters/digits/`-`/`_`/non-ASCII. This is
/// the test esbuild uses to decide whether an attribute-selector value may drop its
/// quotes (`[data-state="open"]` → `[data-state=open]`, but `"a b"`/`"1n"` keep theirs).
/// A leading `-` must be followed by a non-digit to be a valid ident start (`-x` ok,
/// `-1` not), and an empty string is not an identifier.
fn is_css_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let is_name_start = |c: char| c.is_ascii_alphabetic() || c == '_' || !c.is_ascii();
    let is_name = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii();
    let valid_first = if first == '-' {
        // `-` followed by a name-start char or another `-` (`--custom`) is a valid start.
        match chars.clone().next() {
            Some(c) => is_name_start(c) || c == '-',
            None => false, // a lone `-` is not an identifier
        }
    } else {
        is_name_start(first)
    };
    if !valid_first {
        return false;
    }
    s.chars().skip(1).all(is_name)
}

/// Whether the whitespace token at `i` is significant in SELECTOR context — kept
/// rather than dropped. Two cases are significant:
///   * a descendant combinator: a non-space, non-punctuation token on BOTH sides
///     (`.a .b`);
///   * the single space ngc's `ShadowCss` puts AFTER a selector-list comma
///     (`.a:hover, .b`) — ShadowCss re-joins selector lists with `, ` (comma+SPACE),
///     and ng-packagr preserves that space, so packagr must NOT tighten it back to
///     `,`. (A space BEFORE a comma is still dropped: `.a , .b` → `.a, .b`.)
fn space_is_significant(tokens: &[Tok], i: usize) -> bool {
    let prev = prev_significant(tokens, i);
    let next = next_significant(tokens, i);
    // ShadowCss-style selector-list spacing: keep the space that follows a comma.
    // The comma itself is the list separator; the following ` ` is part of `, `.
    if matches!(prev, Some(Tok::Punct(','))) && !matches!(next, Some(Tok::Punct('}'))) {
        return true;
    }
    fn keeps_space(tok: Option<&Tok>) -> bool {
        match tok {
            Some(Tok::Word(_)) | Some(Tok::Str(_)) => true,
            // A space directly inside/adjacent to a combinator (`>`, `+`, `~`) is
            // dropped — handled because those live in Word tokens; keep the
            // surrounding spaces only between bare words.
            _ => false,
        }
    }
    keeps_space(prev) && keeps_space(next)
}

fn prev_significant(tokens: &[Tok], i: usize) -> Option<&Tok> {
    tokens[..i].iter().rev().find(|t| !matches!(t, Tok::Space))
}

fn next_significant(tokens: &[Tok], i: usize) -> Option<&Tok> {
    tokens[i + 1..].iter().find(|t| !matches!(t, Tok::Space))
}

/// A coarse CSS token: either a run of "word" bytes (identifiers, numbers, hex,
/// function calls kept whole), a string literal (normalized to double quotes), or a
/// single structural punctuation char (`{ } : ; , ( )` and combinators).
#[derive(Debug, Clone)]
enum Tok {
    Word(String),
    Str(String),
    Punct(char),
    /// A run of insignificant whitespace (collapsed to a single space, then often
    /// dropped by the renderer when adjacent to punctuation).
    Space,
}

/// Tokenize CSS into a coarse stream. Strings and `url(...)`/`func(...)` parentheses
/// are kept whole so commas/colons inside them never split a declaration. Comments
/// are stripped (esbuild drops them under minify).
fn tokenize(css: &str) -> Vec<Tok> {
    let bytes: Vec<char> = css.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            '/' if i + 1 < bytes.len() && bytes[i + 1] == '*' => {
                // Skip a `/* … */` comment.
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == '*' && bytes[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            c if c.is_whitespace() => {
                while i < bytes.len() && bytes[i].is_whitespace() {
                    i += 1;
                }
                toks.push(Tok::Space);
            }
            '"' | '\'' => {
                let quote = c;
                let mut s = String::new();
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == '\\' && i + 1 < bytes.len() {
                        s.push(bytes[i]);
                        s.push(bytes[i + 1]);
                        i += 2;
                        continue;
                    }
                    s.push(bytes[i]);
                    i += 1;
                }
                i += 1; // closing quote
                toks.push(Tok::Str(s));
            }
            '{' | '}' | ':' | ';' | ',' => {
                toks.push(Tok::Punct(c));
                i += 1;
            }
            '(' => {
                // Keep a parenthesized group whole (function args, url()).
                let mut depth = 1;
                let mut s = String::from("(");
                i += 1;
                while i < bytes.len() && depth > 0 {
                    let ch = bytes[i];
                    if ch == '(' {
                        depth += 1;
                    } else if ch == ')' {
                        depth -= 1;
                        if depth == 0 {
                            s.push(')');
                            i += 1;
                            break;
                        }
                    }
                    s.push(ch);
                    i += 1;
                }
                // Tighten whitespace inside the group (around commas) so a
                // functional notation matches esbuild's `rgb(170,187,204)` form,
                // then append onto the preceding word so `rgb` + `(…)` stays one
                // value token.
                let tightened = tighten_parens(&s);
                if let Some(Tok::Word(w)) = toks.last_mut() {
                    w.push_str(&tightened);
                } else {
                    toks.push(Tok::Word(tightened));
                }
            }
            _ => {
                // A word: identifier / number / hex / operator chars.
                let mut s = String::new();
                while i < bytes.len() {
                    let ch = bytes[i];
                    if ch.is_whitespace()
                        || matches!(ch, '{' | '}' | ':' | ';' | ',' | '(' | ')' | '"' | '\'')
                        || (ch == '/' && i + 1 < bytes.len() && bytes[i + 1] == '*')
                    {
                        break;
                    }
                    s.push(ch);
                    i += 1;
                }
                if !s.is_empty() {
                    toks.push(Tok::Word(s));
                }
            }
        }
    }
    toks
}

/// Collapse insignificant whitespace inside a captured `(...)` functional group:
/// drop spaces adjacent to commas / parens and collapse internal runs to a single
/// space (`rgb(170, 187, 204)` → `rgb(170,187,204)`, `calc(1px + 2px)` →
/// `calc(1px + 2px)` — spaces around `+`/`-` are kept since `calc` needs them).
fn tighten_parens(group: &str) -> String {
    let mut out = String::with_capacity(group.len());
    let chars: Vec<char> = group.chars().collect();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        if c.is_whitespace() {
            // Collapse the run.
            let mut j = k;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            let prev = out.chars().last();
            let next = chars.get(j).copied();
            let drop = matches!(prev, Some('(') | Some(',')) || matches!(next, Some(')') | Some(','));
            if !drop && prev.is_some() && next.is_some() {
                out.push(' ');
            }
            k = j;
            continue;
        }
        out.push(c);
        k += 1;
    }
    out
}

/// At block depth, a declaration starts at the first non-space token after a `{`,
/// `;`, or `}`-less boundary — i.e. a `Word` that is followed (ignoring spaces) by a
/// `:`. We approximate: a `Word` token at the current position whose next
/// significant token is `:`.
fn is_declaration_start(tokens: &[Tok], i: usize) -> bool {
    if !matches!(&tokens[i], Tok::Word(_)) {
        return false;
    }
    // Look ahead for `:` before any `{`/`;`/`}` — that marks `prop:value`.
    let mut j = i + 1;
    while j < tokens.len() {
        match &tokens[j] {
            Tok::Space => j += 1,
            Tok::Punct(':') => return true,
            _ => return false,
        }
    }
    false
}

/// Consume a full `<prop> : <value...> [; | }]` declaration starting at `i`,
/// returning the rendered (minified) declaration text and the index just past it
/// (pointing at the terminating `;` or `}`; the `;` itself is rendered, the `}` is
/// left for the outer loop).
fn take_declaration(tokens: &[Tok], i: usize) -> (String, usize) {
    // property
    let Tok::Word(prop_raw) = &tokens[i] else {
        return (render_selector_token(&tokens[i]), i + 1);
    };
    // A CSS custom property (`--x`) is CASE-SENSITIVE and its value is NOT a typed
    // value — esbuild leaves it completely verbatim (no color/number/hex
    // minification). Keep the authored property name as-is for those.
    let is_custom_property = prop_raw.starts_with("--");
    let prop = if is_custom_property {
        prop_raw.clone()
    } else {
        prop_raw.to_ascii_lowercase()
    };
    let mut j = i + 1;
    // skip spaces + the colon
    while j < tokens.len() && matches!(tokens[j], Tok::Space) {
        j += 1;
    }
    // colon
    if j < tokens.len() && matches!(tokens[j], Tok::Punct(':')) {
        j += 1;
    }
    // Whether whitespace separates the colon from the first value token. esbuild
    // preserves a single such space for custom properties (`--x: #336699`), so we
    // must record it before skipping spaces.
    let space_after_colon = matches!(tokens.get(j), Some(Tok::Space));
    // collect value tokens until `;` or `}`
    let mut value_tokens: Vec<&Tok> = Vec::new();
    while j < tokens.len() {
        match &tokens[j] {
            Tok::Punct(';') => {
                j += 1; // consume the semicolon
                break;
            }
            Tok::Punct('}') => break, // leave the brace for the outer loop
            t => {
                value_tokens.push(t);
                j += 1;
            }
        }
    }

    let rendered = if is_custom_property {
        // VERBATIM custom-property value (esbuild never minifies `--x` values): the
        // internal whitespace is collapsed to single spaces but the tokens are
        // untouched, and the single space after the colon is preserved when authored.
        let value = render_custom_property_value(&value_tokens);
        let sep = if space_after_colon { " " } else { "" };
        format!("{prop}:{sep}{value};")
    } else {
        let value = render_value(&prop, &value_tokens);
        format!("{prop}:{value};")
    };
    (rendered, j)
}

/// Render a CSS custom-property (`--x`) value VERBATIM, matching esbuild's
/// `minify: true` behaviour for custom properties: it collapses internal whitespace
/// runs to a single space but performs NO value transforms — the hex stays full-width
/// (`#336699`, not `#369`), `0px` keeps its unit, `bold` stays a keyword, because the
/// value of a custom property is an arbitrary token stream, not a typed CSS value.
fn render_custom_property_value(tokens: &[&Tok]) -> String {
    let mut pieces: Vec<String> = Vec::new();
    for t in tokens {
        match t {
            Tok::Space => { /* collapsed; single spaces reinserted on join */ }
            Tok::Word(w) => pieces.push(w.clone()),
            Tok::Str(s) => pieces.push(format!("\"{}\"", normalize_string(s))),
            Tok::Punct(c) => pieces.push(c.to_string()),
        }
    }
    pieces.join(" ")
}

/// Render a declaration value: trim, collapse internal whitespace to single spaces,
/// and apply per-token value transforms with property awareness.
fn render_value(prop: &str, tokens: &[&Tok]) -> String {
    // Build a list of significant value pieces (words/strings), single-spaced.
    let mut pieces: Vec<String> = Vec::new();
    for t in tokens {
        match t {
            Tok::Space => { /* collapsed; word boundaries reinserted below */ }
            Tok::Word(w) => pieces.push(w.clone()),
            Tok::Str(s) => pieces.push(format!("\"{}\"", normalize_string(s))),
            Tok::Punct(c) => {
                // A comma inside a value (e.g. font-family list, transition list).
                if *c == ',' {
                    pieces.push(",".to_string());
                } else {
                    pieces.push(c.to_string());
                }
            }
        }
    }

    // Normalize a trailing `!important` (esbuild tightens `red !important` and
    // `red ! important` → `red!important`): merge a bare `!` with a following
    // `important` keyword into a single `!important` piece. The no-space join is
    // handled below (a piece starting with `!` gets no leading space).
    let mut merged: Vec<String> = Vec::with_capacity(pieces.len());
    let mut p = 0;
    while p < pieces.len() {
        if pieces[p] == "!"
            && pieces
                .get(p + 1)
                .is_some_and(|n| n.eq_ignore_ascii_case("important"))
        {
            merged.push(format!("!{}", pieces[p + 1].to_ascii_lowercase()));
            p += 2;
        } else {
            merged.push(pieces[p].clone());
            p += 1;
        }
    }
    let pieces = merged;

    // Whether this declaration has exactly one value token (a sole color/keyword),
    // which is the only position where we apply name→hex / keyword→number, matching
    // esbuild's "convert only in a color-typed single position" behaviour. A trailing
    // `!important` does not count toward the value-token total.
    let value_piece_count = pieces
        .iter()
        .filter(|p| !p.starts_with('!'))
        .count();
    let single = value_piece_count == 1;

    let mut out_pieces: Vec<String> = Vec::with_capacity(pieces.len());
    for piece in &pieces {
        // Leave a `!important` flag untouched (it is not a typed value token).
        if piece.starts_with('!') {
            out_pieces.push(piece.clone());
        } else {
            out_pieces.push(transform_value_token(prop, piece, single));
        }
    }

    // Join with single spaces, but no space before/after a comma and no space
    // before a `!important` flag (`red!important`).
    let mut s = String::new();
    for (idx, p) in out_pieces.iter().enumerate() {
        if idx > 0 {
            let prev_comma = out_pieces[idx - 1] == ",";
            let this_comma = p == ",";
            let this_important = p.starts_with('!');
            if !prev_comma && !this_comma && !this_important {
                s.push(' ');
            }
        }
        s.push_str(p);
    }
    s
}

/// The set of longhand properties whose value is a single `<color>` — the positions
/// where esbuild always converts a color *name* to its shortest hex. Shorthands
/// (`border`, `background`, `outline`, `box-shadow`) are intentionally excluded.
fn is_color_only_property(prop: &str) -> bool {
    matches!(
        prop,
        "color"
            | "background-color"
            | "border-color"
            | "border-top-color"
            | "border-right-color"
            | "border-bottom-color"
            | "border-left-color"
            | "outline-color"
            | "text-decoration-color"
            | "caret-color"
            | "fill"
            | "stroke"
            | "stop-color"
            | "flood-color"
            | "column-rule-color"
    )
}

/// Apply value-token transforms: hex normalization (anywhere), number
/// normalization (anywhere), and — only for a single-token value — `font-weight`
/// keyword→number and color-only-property name→hex.
fn transform_value_token(prop: &str, token: &str, single: bool) -> String {
    // Hex colors: normalize wherever they appear (a hex literal is unambiguous).
    if let Some(hex) = normalize_hex(token) {
        return hex;
    }

    // Numbers with optional units: normalize wherever they appear.
    if let Some(num) = normalize_number(token) {
        return num;
    }

    // `transform`/`translate` function simplification: a single-axis `translateX(<v>)`
    // collapses to `translate(<v>)` (esbuild rewrites the one-arg X form to the shorter
    // `translate()`; `translateY`/`translateZ`/`scaleX`/… are left as-is). Applied to any
    // value-token regardless of position so it covers a multi-function `transform` list
    // (`translateX(10px) rotate(45deg)`).
    if let Some(simplified) = simplify_translate_x(token) {
        return simplified;
    }

    // Zero-angle transform rotation/skew: a `rotate(0deg)`/`rotateZ(0deg)`/`skew(0deg)`
    // (any zero angle unit) collapses to `rotate(0)`/`skew(0)` — esbuild evaluates the
    // zero angle to a unitless `0` inside transform rotation functions (it does NOT do
    // this in a non-transform context like `linear-gradient(0deg, …)`, so this is keyed
    // on the transform-function name).
    if let Some(simplified) = simplify_zero_angle_transform(token) {
        return simplified;
    }

    if single {
        // font-weight keyword → number.
        if prop == "font-weight" {
            match token.to_ascii_lowercase().as_str() {
                "normal" => return "400".to_string(),
                "bold" => return "700".to_string(),
                _ => {}
            }
        }
        // Color name → shortest hex, only in a color-only longhand position.
        if is_color_only_property(prop)
            && let Some(hex) = color_name_to_short_hex(token)
        {
            return hex;
        }
    }

    token.to_string()
}

/// Simplify a single-axis `translateX(<v>)` value-function to `translate(<v>)` — esbuild's
/// `minify: true` transform-function shortening (the one-argument X form is equivalent to the
/// two-argument `translate()` with an implicit `0` Y). Returns `None` for anything else.
///
/// Conservative, to never diverge from esbuild:
///   * ONLY `translateX` is rewritten (case-insensitive name) — esbuild leaves `translateY`,
///     `translateZ`, `scaleX`, … untouched; only the X axis maps onto the shorter `translate()`.
///   * ONLY the single-argument form (no top-level comma) is rewritten.
///   * The single argument must be a simple value esbuild serializes identically (a number/length/
///     percentage, which we normalize the same way — `0px`→`0`, `0.5px`→`.5px` — or a `var(...)` /
///     bare identifier left verbatim). A `calc(...)` / multi-token argument is NOT rewritten,
///     because esbuild may re-serialize it (e.g. evaluate `calc`) and a blind rename would diverge.
fn simplify_translate_x(token: &str) -> Option<String> {
    // Split `<fn>(<args>)` — the function name (ASCII letters) then a parenthesized argument list.
    let open = token.find('(')?;
    if !token.ends_with(')') {
        return None;
    }
    let fn_name = &token[..open];
    if !fn_name.eq_ignore_ascii_case("translateX") {
        return None;
    }
    let inner = &token[open + 1..token.len() - 1];
    let arg = inner.trim();
    // Single argument only (no top-level comma — a two-arg `translateX(a,b)` is invalid for X but we
    // still must not rewrite it). A nested function's commas are inside its own parens; a TOP-level
    // comma here means more than one argument.
    if has_top_level_comma(arg) {
        return None;
    }
    if arg.is_empty() {
        return None;
    }
    // Normalize the argument exactly as esbuild does for a standalone value (number/length), else
    // keep it verbatim (a `var(...)` / identifier). If the arg is a number we recognise, normalize
    // it; otherwise rewrite only when it is a single bareword/`var()` token (no internal spaces),
    // which esbuild passes through unchanged.
    let normalized_arg = if let Some(num) = normalize_number(arg) {
        num
    } else if arg.contains(char::is_whitespace) {
        // A multi-token / `calc(... + ...)` argument: esbuild may re-serialize it. Stay conservative.
        return None;
    } else {
        arg.to_string()
    };
    Some(format!("translate({normalized_arg})"))
}

/// Collapse a zero-angle transform rotation/skew to its unitless form:
/// `rotate(0deg)` / `rotate(0grad)` / `rotateZ(0turn)` → `rotate(0)`, `skew(0deg)` →
/// `skew(0)`. esbuild evaluates a zero CSS `<angle>` argument of a transform rotation
/// function to a unitless `0` (and folds `rotateZ` onto `rotate`). Returns `None` for
/// any non-rotation function, a non-zero angle, or a multi-argument call.
///
/// Conservative: only the transform rotation/skew functions are matched (esbuild does
/// NOT strip the unit in a non-transform context such as `linear-gradient(0deg, …)`),
/// and only a single zero-angle argument is rewritten.
fn simplify_zero_angle_transform(token: &str) -> Option<String> {
    let open = token.find('(')?;
    if !token.ends_with(')') {
        return None;
    }
    let fn_name = &token[..open];
    // `rotateZ` serializes to `rotate` (the Z axis is the default rotation axis).
    let out_name = if fn_name.eq_ignore_ascii_case("rotate")
        || fn_name.eq_ignore_ascii_case("rotateZ")
    {
        "rotate"
    } else if fn_name.eq_ignore_ascii_case("skew") {
        "skew"
    } else {
        return None;
    };
    let arg = token[open + 1..token.len() - 1].trim();
    if has_top_level_comma(arg) {
        return None;
    }
    if !is_zero_angle(arg) {
        return None;
    }
    Some(format!("{out_name}(0)"))
}

/// Whether `arg` is a zero CSS `<angle>` (`0`, `0deg`, `0grad`, `0rad`, `0turn`, in any
/// case), i.e. a numerically-zero value carrying an angle unit (or no unit).
fn is_zero_angle(arg: &str) -> bool {
    let lower = arg.to_ascii_lowercase();
    let num = lower
        .strip_suffix("deg")
        .or_else(|| lower.strip_suffix("grad"))
        .or_else(|| lower.strip_suffix("rad"))
        .or_else(|| lower.strip_suffix("turn"))
        .unwrap_or(&lower);
    // The numeric head must parse to exactly zero (`0`, `0.0`, `.0`, `00`).
    num.parse::<f64>().map(|v| v == 0.0).unwrap_or(false)
}

/// Whether `s` contains a comma OUTSIDE any parentheses (a top-level argument separator). Commas
/// nested inside a `(...)` group (e.g. `var(--x, 0)`) do not count.
fn has_top_level_comma(s: &str) -> bool {
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => return true,
            _ => {}
        }
    }
    false
}

/// Normalize a hex color token (`#RRGGBB`/`#RGB`/`#RRGGBBAA`/`#RGBA`): lowercase,
/// and shorten a 6-digit value to 3 (or 8→4) when each channel's two hex digits are
/// equal. Returns `None` if `token` is not a hex color.
fn normalize_hex(token: &str) -> Option<String> {
    let rest = token.strip_prefix('#')?;
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let lower = rest.to_ascii_lowercase();
    let shortened = match lower.len() {
        6 => shorten_hex_pairs(&lower, 3),
        8 => shorten_hex_pairs(&lower, 4),
        _ => None,
    };
    Some(format!("#{}", shortened.unwrap_or(lower)))
}

/// If every adjacent pair of the `count` channels in `hex` is a doubled digit
/// (`aa`, `bb`, …), return the `count`-digit short form; else `None`.
fn shorten_hex_pairs(hex: &str, count: usize) -> Option<String> {
    let chars: Vec<char> = hex.chars().collect();
    let mut short = String::with_capacity(count);
    for k in 0..count {
        let a = chars[2 * k];
        let b = chars[2 * k + 1];
        if a != b {
            return None;
        }
        short.push(a);
    }
    Some(short)
}

/// Normalize a numeric token with an optional unit: drop a leading integer zero
/// (`0.5`→`.5`), trailing fractional zeros (`0.50`→`.5`, `10.0`→`10`), and the unit
/// on a zero *length* (`0px`→`0`) while keeping a zero percentage/time/angle unit
/// (`0%`, `0s`, `0deg`). Returns `None` if `token` is not a number(+unit).
fn normalize_number(token: &str) -> Option<String> {
    // Optional leading sign.
    let (sign, body) = match token.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", token),
    };
    // Split the numeric head from a trailing unit (alpha/`%`).
    let mut split = 0;
    let chars: Vec<char> = body.chars().collect();
    while split < chars.len() && (chars[split].is_ascii_digit() || chars[split] == '.') {
        split += 1;
    }
    if split == 0 {
        return None; // no numeric head → not a number.
    }
    let num_str: String = chars[..split].iter().collect();
    let unit: String = chars[split..].iter().collect();

    // Must be a valid number (at most one dot, at least one digit).
    if num_str.matches('.').count() > 1 || !num_str.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }
    // A unit, if present, must be alphabetic or `%` (else this is not a value we
    // recognise, e.g. a hash or identifier that began with a digit).
    if !unit.is_empty() && !(unit == "%" || unit.chars().all(|c| c.is_ascii_alphabetic())) {
        return None;
    }

    let normalized_num = normalize_number_literal(&num_str);

    // Is the numeric value exactly zero?
    let is_zero = normalized_num == "0";

    // A zero LENGTH drops its unit (`0px`→`0`); a zero percentage/time/etc keeps it
    // (`0%` stays, `0s` stays — matching esbuild, which only strips length units).
    if is_zero {
        if unit.is_empty() {
            return Some(format!("{sign}0"));
        }
        if is_length_unit(&unit) {
            return Some(format!("{sign}0"));
        }
        return Some(format!("{sign}0{unit}"));
    }

    Some(format!("{sign}{normalized_num}{unit}"))
}

/// Normalize a bare numeric literal (no unit): strip a leading integer zero before
/// a dot (`0.5`→`.5`), strip trailing fractional zeros (`0.50`→`.5`, `1.0`→`1`),
/// and collapse a bare `0`/`0.0` to `0`.
fn normalize_number_literal(num: &str) -> String {
    if !num.contains('.') {
        // Integer: strip nothing except a redundant all-zero (`00`→`0` is not done
        // by esbuild — `00.5` stays `00.5` — so leave integers verbatim).
        return num.to_string();
    }
    let (int_part, frac_part) = num.split_once('.').unwrap();
    // Trim trailing zeros from the fraction.
    let frac_trimmed = frac_part.trim_end_matches('0');
    if frac_trimmed.is_empty() {
        // `10.0` → `10`, `0.0` → `0`.
        let int_norm = if int_part.is_empty() { "0" } else { int_part };
        return int_norm.to_string();
    }
    // Drop a single leading integer zero (`0.5`→`.5`), but keep `00.5` as-is to
    // mirror esbuild (it only drops a lone leading zero).
    let int_norm = if int_part == "0" { "" } else { int_part };
    format!("{int_norm}.{frac_trimmed}")
}

/// Whether `unit` is a CSS length unit (the units esbuild strips on a zero value).
fn is_length_unit(unit: &str) -> bool {
    matches!(
        unit.to_ascii_lowercase().as_str(),
        "px" | "em"
            | "rem"
            | "ex"
            | "ch"
            | "vw"
            | "vh"
            | "vmin"
            | "vmax"
            | "cm"
            | "mm"
            | "in"
            | "pt"
            | "pc"
            | "q"
    )
}

/// Map a CSS color *name* to its shortest hex form, but ONLY when the hex is no
/// longer than the name (esbuild keeps `red`/`green` as names because the name is
/// shorter; it converts `blue`→`#00f`, `white`→`#fff`, etc.).
fn color_name_to_short_hex(token: &str) -> Option<String> {
    let name = token.to_ascii_lowercase();
    // (name, full 6-digit hex). The shortener then collapses to 3 digits when
    // possible; we only return the result if it is shorter than the name.
    let hex6 = match name.as_str() {
        "white" => "ffffff",
        "black" => "000000",
        "blue" => "0000ff",
        "cyan" => "00ffff",
        "aqua" => "00ffff",
        "magenta" => "ff00ff",
        "fuchsia" => "ff00ff",
        "yellow" => "ffff00",
        "red" => "ff0000",
        "lime" => "00ff00",
        "gray" => "808080",
        "grey" => "808080",
        "green" => "008000",
        "maroon" => "800000",
        "navy" => "000080",
        "olive" => "808000",
        "purple" => "800080",
        "silver" => "c0c0c0",
        "teal" => "008080",
        _ => return None,
    };
    let short = shorten_hex_pairs(hex6, 3).unwrap_or_else(|| hex6.to_string());
    let candidate = format!("#{short}");
    // esbuild rewrites a color name to hex when the hex is no LONGER than the name:
    //   `blue` (4) → `#00f` (4)   — converted (equal length)
    //   `red`  (3) vs `#f00` (4)  — kept     (hex is longer)
    //   `white`(5) → `#fff` (4)   — converted
    // So the condition is `hex.len() <= name.len()`.
    if candidate.len() <= name.len() {
        Some(candidate)
    } else {
        None
    }
}

/// Normalize a CSS string literal's interior. esbuild re-serializes CSS strings
/// with double quotes; the interior is kept verbatim (this is CSS-level text — JS
/// string escaping for the enclosing `styles: ['…']` literal is the caller's job).
/// An interior double quote is rewritten to its CSS escape (`\"`) so the CSS string
/// stays well-formed when wrapped in double quotes.
fn normalize_string(s: &str) -> String {
    // A bare interior `"` would terminate the double-quoted CSS string; CSS-escape
    // it. (An empty `''` → `""` is the common case and needs no change.)
    s.replace('"', "\\\"")
}

/// Render a non-declaration token (selector text, at-rule prelude) tightened: a
/// `Space` between two structural tokens is dropped; a `Space` between two words is
/// kept as a single space (descendant combinator). Punctuation is emitted verbatim.
fn render_selector_token(tok: &Tok) -> String {
    match tok {
        Tok::Word(w) => w.clone(),
        Tok::Str(s) => format!("\"{}\"", normalize_string(s)),
        Tok::Punct(c) => c.to_string(),
        Tok::Space => " ".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(css: &str) -> String {
        optimize(css)
    }

    #[test]
    fn collapses_whitespace_and_minifies_values() {
        let input = ".box {\n  color: blue;\n  font-weight: bold;\n  background: #ffffff;\n  margin: 0px;\n  padding: 0.500em;\n}";
        let out = opt(input);
        // Matches the ng-packagr@21 FESM value minification on the color-only / number
        // cases (the same context-free transforms esbuild does pre-scope).
        assert!(out.contains("color:#00f"), "color not minified: {out}");
        assert!(out.contains("font-weight:700"), "font-weight not minified: {out}");
        assert!(out.contains("background:#fff"), "hex not shortened: {out}");
        assert!(out.contains("margin:0"), "zero unit not stripped: {out}");
        assert!(!out.contains("0px"), "0px should drop unit: {out}");
        assert!(out.contains("padding:.5em"), "leading zero / trailing zeros: {out}");
        // Whitespace collapsed: no double spaces or newlines.
        assert!(!out.contains('\n'), "newlines remain: {out}");
        assert!(!out.contains("  "), "double spaces remain: {out}");
    }

    #[test]
    fn preserves_ngcontent_placeholder() {
        let input = ".box[_ngcontent-%COMP%] {\n  color: blue;\n}";
        let out = opt(input);
        assert!(out.contains("[_ngcontent-%COMP%]"), "placeholder lost: {out}");
        assert_eq!(out, ".box[_ngcontent-%COMP%]{color:#00f}");
    }

    #[test]
    fn preserves_nghost_placeholder() {
        let input = "[_nghost-%COMP%] {\n  display: block;\n  color: red;\n}";
        let out = opt(input);
        assert!(out.contains("[_nghost-%COMP%]"), "host placeholder lost: {out}");
        // `red` is shorter than `#f00`, so ng-packagr keeps the name (esbuild's
        // shortest-form rule, preserved into the FESM).
        assert!(out.contains("color:red"), "red should stay a name: {out}");
    }

    #[test]
    fn matches_scoped_box_exactly() {
        // The exact scoped CSS treaty_ivy emits for the styled-box fixture.
        let input = ".box[_ngcontent-%COMP%] {\n      color: blue;\n      font-weight: bold;\n      background: #ffffff;\n      margin: 0px;\n      padding: 0.500em;\n      border: 1px solid rgb(170, 187, 204);\n    }";
        let out = opt(input);
        // ng-packagr keeps the color inside the `border` shorthand AND keeps rgb()
        // there verbatim (the FESM does not re-serialize the shorthand). We match that:
        // border value stays `1px solid rgb(170,187,204)` (whitespace tightened).
        assert_eq!(
            out,
            ".box[_ngcontent-%COMP%]{color:#00f;font-weight:700;background:#fff;margin:0;padding:.5em;border:1px solid rgb(170,187,204)}"
        );
    }

    #[test]
    fn keeps_color_inside_border_shorthand_verbatim() {
        // ng-packagr's FESM does NOT convert a color name inside the `border`
        // shorthand (esbuild only converts colors in `<color>`-typed positions).
        let out = opt(".a { border: 1px solid blue; }");
        assert_eq!(out, ".a{border:1px solid blue}");
    }

    #[test]
    fn keeps_zero_percent_and_zero_time() {
        let out = opt(".a { width: 0%; transition-delay: 0s; }");
        assert!(out.contains("width:0%"), "0% must keep unit: {out}");
        assert!(out.contains("transition-delay:0s"), "0s must keep unit: {out}");
    }

    #[test]
    fn pseudo_class_colon_not_split() {
        // A `:hover` in the selector must not be parsed as a declaration.
        let out = opt(".a:hover { color: blue; }");
        assert_eq!(out, ".a:hover{color:#00f}");
    }

    #[test]
    fn font_family_list_keeps_commas() {
        let out = opt(".a { font-family: Arial , sans-serif ; }");
        assert_eq!(out, ".a{font-family:Arial,sans-serif}");
    }

    #[test]
    fn hex_eight_digit_shortens_to_four() {
        assert_eq!(normalize_hex("#11223344").as_deref(), Some("#1234"));
        assert_eq!(normalize_hex("#AABBCC").as_deref(), Some("#abc"));
        assert_eq!(normalize_hex("#abcd").as_deref(), Some("#abcd"));
        assert_eq!(normalize_hex("#123456").as_deref(), Some("#123456"));
    }

    #[test]
    fn number_normalization_cases() {
        assert_eq!(normalize_number("0px").as_deref(), Some("0"));
        assert_eq!(normalize_number("0.0px").as_deref(), Some("0"));
        assert_eq!(normalize_number("0.5px").as_deref(), Some(".5px"));
        assert_eq!(normalize_number("0.50px").as_deref(), Some(".5px"));
        assert_eq!(normalize_number("10.0px").as_deref(), Some("10px"));
        assert_eq!(normalize_number("0%").as_deref(), Some("0%"));
        assert_eq!(normalize_number("0.5").as_deref(), Some(".5"));
    }

    #[test]
    fn empty_string_content_is_double_quoted() {
        let out = opt(".a { content: ''; }");
        assert_eq!(out, ".a{content:\"\"}");
    }

    // -----------------------------------------------------------------------
    // ng-packagr@21 micro-opt parity: transform-fn simplification, selector-list
    // comma, and @media at-rule handling. The ORACLE is the ng-packagr@21 FESM
    // `styles` bytes (esbuild minifies pre-scope, ngc `ShadowCss` then scopes, and the
    // FESM is emitted). Each expectation below reproduces those captured FESM bytes.
    // -----------------------------------------------------------------------

    /// A single-axis `translateX(<v>)` collapses to `translate(<v>)` — esbuild's transform-function
    /// shortening, which survives scoping into the ng-packagr FESM. Only `translateX` is rewritten
    /// (the X axis maps onto the shorter two-arg `translate()`); the inner length is normalized the
    /// same way a standalone value is.
    #[test]
    fn translate_x_collapses_to_translate() {
        // ng-packagr FESM: `.a{transform:translate(10px)}`
        assert_eq!(opt(".a { transform: translateX(10px); }"), ".a{transform:translate(10px)}");
        // Inner length normalized: `0px`→`0`, `0.5px`→`.5px`.
        assert_eq!(opt(".a { transform: translateX(0px); }"), ".a{transform:translate(0)}");
        assert_eq!(opt(".a { transform: translateX(0.5px); }"), ".a{transform:translate(.5px)}");
        // A multi-function `transform` list: only the X function is rewritten.
        assert_eq!(
            opt(".a { transform: translateX(10px) rotate(45deg); }"),
            ".a{transform:translate(10px) rotate(45deg)}"
        );
        // Case-insensitive function name; surrounding arg whitespace dropped.
        assert_eq!(opt(".a { transform: TRANSLATEX( 10px ); }"), ".a{transform:translate(10px)}");
        // A `var(...)` / percentage argument passes through (ng-packagr keeps it verbatim).
        assert_eq!(opt(".a { transform: translateX(var(--x)); }"), ".a{transform:translate(var(--x))}");
        assert_eq!(opt(".a { transform: translateX(50%); }"), ".a{transform:translate(50%)}");
    }

    /// Functions ng-packagr does NOT shorten must be left verbatim (only the X axis maps onto
    /// `translate()`); a two-argument or `calc(...)` argument is also left untouched (esbuild may
    /// re-serialize those — a blind rename would diverge, so packagr stays conservative).
    #[test]
    fn other_transform_fns_are_left_verbatim() {
        // ng-packagr keeps these unchanged.
        assert_eq!(opt(".a { transform: translateY(10px); }"), ".a{transform:translateY(10px)}");
        assert_eq!(opt(".a { transform: translateZ(10px); }"), ".a{transform:translateZ(10px)}");
        assert_eq!(opt(".a { transform: scaleX(2); }"), ".a{transform:scaleX(2)}");
        // Two-arg `translateX(a,b)` is not the single-axis form → not rewritten.
        assert_eq!(opt(".a { transform: translateX(10px,20px); }"), ".a{transform:translateX(10px,20px)}");
        // `calc(...)` argument: packagr does not evaluate calc, so it leaves the whole function
        // untouched rather than emitting a rename that would diverge from the FESM's evaluated form.
        assert_eq!(
            opt(".a { transform: translateX(calc(10px + 2px)); }"),
            ".a{transform:translateX(calc(10px + 2px))}"
        );
    }

    /// Selector-list commas KEEP the single space after the comma (`.a:hover, .b`) — because
    /// ngc's `ShadowCss` re-joins selector lists with `, ` (comma+SPACE) and ng-packagr's FESM
    /// preserves it. This is a REVERSAL of the prior esbuild-only behaviour: a bare
    /// `esbuild.transformSync(minify:true)` produces `.a:hover,.b` (no space), but the real
    /// pipeline scopes AFTER esbuild, so the comma+space survives. A space BEFORE the comma is
    /// still dropped (`.a , .b` → `.a, .b`).
    #[test]
    fn selector_list_comma_keeps_following_space() {
        assert_eq!(opt(".a:hover, .b { color: blue; }"), ".a:hover, .b{color:#00f}");
        assert_eq!(opt(".a , .b , .c { color: red; }"), ".a, .b, .c{color:red}");
        // A list authored tight stays tight (there is no space to preserve).
        assert_eq!(opt("div,p,span { color: blue; }"), "div,p,span{color:#00f}");
        // Mixed: only the authored space after the comma is kept.
        assert_eq!(opt("div, p,span { color: blue; }"), "div, p,span{color:#00f}");
    }

    /// `@media` feature-query preludes are TIGHTENED to match the ng-packagr@21 FESM:
    /// `@media (min-width: 600px)` → `@media(min-width:600px)` (no space after `@media`, no space
    /// after the colon). This is a REVERSAL of the prior "leave it loose" behaviour: a bare
    /// `esbuild.transformSync(minify:true)`, ng-packagr's own exact esbuild `build` invocation, AND
    /// `encapsulateStyle(esbuildBundleFile(css))` ALL leave the prelude LOOSE — but the emitted FESM
    /// is TIGHT, and the FESM is the oracle. `@supports` is NOT a `@media`-family at-rule, so its
    /// prelude stays loose (asserted separately).
    #[test]
    fn media_query_prelude_tightened_to_ng_packagr() {
        // ng-packagr@21 FESM (captured): `@media(min-width:600px){…}`.
        assert_eq!(
            opt("@media (min-width: 600px) { .a { color: blue; } }"),
            "@media(min-width:600px){.a{color:#00f}}"
        );
        // The `screen and ` keyword chain keeps its spaces; only the parenthesized feature query
        // is tightened, and the `@media`↔`(` space is dropped only when `(` follows `@media`.
        // Captured from a real ng-packagr build (d:/tmp/css-edge):
        // `@media screen and (min-width:600px){…}`.
        assert_eq!(
            opt("@media screen and (min-width: 600px) { .a { color: blue; } }"),
            "@media screen and (min-width:600px){.a{color:#00f}}"
        );
        // Nested @media: outer `@media screen` (no paren) stays loose, inner tightened.
        assert_eq!(
            opt("@media screen { @media (min-width: 900px) { .a { display: flex; } } }"),
            "@media screen{@media(min-width:900px){.a{display:flex}}}"
        );
        // An at-rule prelude comma is tight (`@media screen,print`), captured from real ng-packagr.
        assert_eq!(
            opt("@media screen, print { .a { color: blue; } }"),
            "@media screen,print{.a{color:#00f}}"
        );
    }

    /// `@supports` preludes stay LOOSE in the ng-packagr@21 FESM — the space after `@supports`
    /// AND the space after the colon are both preserved (`@supports (display: grid)`). Only
    /// `@media`-family preludes are tightened.
    #[test]
    fn supports_prelude_stays_loose() {
        assert_eq!(
            opt("@supports (display: grid) { .a { display: grid; } }"),
            "@supports (display: grid){.a{display:grid}}"
        );
        // Captured from real ng-packagr (d:/tmp/css-edge): fully loose, incl. ` and ` join.
        assert_eq!(
            opt("@supports (display: grid) and (gap: 1em) { .a { color: blue; } }"),
            "@supports (display: grid) and (gap: 1em){.a{color:#00f}}"
        );
    }

    // -----------------------------------------------------------------------
    // ng-packagr@21 BYTE-EQUALITY regression guard.
    //
    // `SCOPED_FIXTURE` is the EXACT output of treaty_ivy's `ShadowCss` port on the
    // css-oracle fixture stylesheet (`D:\tmp\css-oracle\lib\src\styled.component.css`),
    // captured by running `shim_css_text(css, "_ngcontent-%COMP%", "_nghost-%COMP%")` —
    // i.e. the real input this optimizer receives in the packagr pipeline. `NG_PACKAGR_FESM`
    // is the EXACT `styles` element ng-packagr@21.2.3 emitted into the FESM for the same
    // fixture (`D:\tmp\css-oracle\lib\dist-ng-full\fesm2022\css-oracle-lib.mjs`). The
    // optimizer must turn the first byte-for-byte into the second. This asserts EVERY
    // construct at once (single rule, selector-list comma+space, @media tighten, nested
    // @media, @supports loose, @keyframes from→0%/rotate(0deg)→rotate(0), combinators,
    // calc loose, custom-prop verbatim, !important tighten, attr-quote strip, ::before→:before).
    // -----------------------------------------------------------------------

    const SCOPED_FIXTURE: &str = r##"
.box[_ngcontent-%COMP%] {
  color: blue;
  font-weight: bold;
  background: #ffffff;
  margin: 0px;
  padding: 0.500em;
}


.box[_ngcontent-%COMP%]:hover, .box.active[_ngcontent-%COMP%] {
  color: red;
  text-decoration: underline;
}


@media (min-width: 600px) {
  .box[_ngcontent-%COMP%] {
    color: green;
  }
}


@media screen {
  @media (min-width: 900px) {
    .box[_ngcontent-%COMP%] {
      display: flex;
    }
  }
}


@supports (display: grid) {
  .box[_ngcontent-%COMP%] {
    display: grid;
  }
}


@keyframes _ngcontent-%COMP%_spin {
  from {
    transform: rotate(0deg);
  }
  to {
    transform: rotate(360deg);
  }
}


.box[_ngcontent-%COMP%] > .child[_ngcontent-%COMP%] {
  color: cyan;
}


.box[_ngcontent-%COMP%] + .sibling[_ngcontent-%COMP%] {
  margin-left: 10px;
}


.box[_ngcontent-%COMP%] ~ .following[_ngcontent-%COMP%] {
  margin-right: 10px;
}


.box.calc[_ngcontent-%COMP%] {
  width: calc(100% - 20px);
}


.box.themed[_ngcontent-%COMP%] {
  --x: #336699;
  color: var(--x);
}


.box.urgent[_ngcontent-%COMP%] {
  color: red !important;
}


.box[data-state="open"][_ngcontent-%COMP%] {
  display: block;
}


.box[_ngcontent-%COMP%]::before {
  content: '';
  display: inline-block;
}"##;

    const NG_PACKAGR_FESM: &str = r#".box[_ngcontent-%COMP%]{color:#00f;font-weight:700;background:#fff;margin:0;padding:.5em}.box[_ngcontent-%COMP%]:hover, .box.active[_ngcontent-%COMP%]{color:red;text-decoration:underline}@media(min-width:600px){.box[_ngcontent-%COMP%]{color:green}}@media screen{@media(min-width:900px){.box[_ngcontent-%COMP%]{display:flex}}}@supports (display: grid){.box[_ngcontent-%COMP%]{display:grid}}@keyframes _ngcontent-%COMP%_spin{0%{transform:rotate(0)}to{transform:rotate(360deg)}}.box[_ngcontent-%COMP%] > .child[_ngcontent-%COMP%]{color:#0ff}.box[_ngcontent-%COMP%] + .sibling[_ngcontent-%COMP%]{margin-left:10px}.box[_ngcontent-%COMP%] ~ .following[_ngcontent-%COMP%]{margin-right:10px}.box.calc[_ngcontent-%COMP%]{width:calc(100% - 20px)}.box.themed[_ngcontent-%COMP%]{--x: #336699;color:var(--x)}.box.urgent[_ngcontent-%COMP%]{color:red!important}.box[data-state=open][_ngcontent-%COMP%]{display:block}.box[_ngcontent-%COMP%]:before{content:"";display:inline-block}"#;

    /// THE byte-equality gate: optimizing the real ShadowCss-scoped fixture must equal
    /// the real ng-packagr@21 FESM `styles` element, byte for byte.
    #[test]
    fn scoped_fixture_is_byte_identical_to_ng_packagr_fesm() {
        assert_eq!(opt(SCOPED_FIXTURE), NG_PACKAGR_FESM);
    }

    /// The selector-list comma+SPACE survives (the construct that was FLIPPED away from
    /// the old esbuild-tightened `,`): ng-packagr keeps `:hover, .box.active`.
    #[test]
    fn ng_byte_selector_list_comma_space_kept() {
        let scoped = ".box[_ngcontent-%COMP%]:hover, .box.active[_ngcontent-%COMP%] { color: red; text-decoration: underline; }";
        assert_eq!(
            opt(scoped),
            ".box[_ngcontent-%COMP%]:hover, .box.active[_ngcontent-%COMP%]{color:red;text-decoration:underline}"
        );
    }

    /// The `@media` feature query is tightened (the construct that was FLIPPED toward
    /// tightening): ng-packagr emits `@media(min-width:600px)`.
    #[test]
    fn ng_byte_media_prelude_tightened() {
        assert_eq!(
            opt("@media (min-width: 600px) { .box[_ngcontent-%COMP%] { color: green; } }"),
            "@media(min-width:600px){.box[_ngcontent-%COMP%]{color:green}}"
        );
        assert_eq!(
            opt("@media screen { @media (min-width: 900px) { .box[_ngcontent-%COMP%] { display: flex; } } }"),
            "@media screen{@media(min-width:900px){.box[_ngcontent-%COMP%]{display:flex}}}"
        );
    }

    /// `@keyframes` `from` → `0%` and `rotate(0deg)` → `rotate(0)`; `to`/`rotate(360deg)` kept.
    #[test]
    fn ng_byte_keyframes_from_and_zero_rotate() {
        let scoped = "@keyframes _ngcontent-%COMP%_spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }";
        assert_eq!(
            opt(scoped),
            "@keyframes _ngcontent-%COMP%_spin{0%{transform:rotate(0)}to{transform:rotate(360deg)}}"
        );
    }

    /// A custom property is left VERBATIM — full 6-digit hex and the space after the colon
    /// preserved (`--x: #336699`); ng-packagr does not minify custom-property values.
    #[test]
    fn ng_byte_custom_property_verbatim() {
        assert_eq!(
            opt(".box.themed[_ngcontent-%COMP%] { --x: #336699; color: var(--x); }"),
            ".box.themed[_ngcontent-%COMP%]{--x: #336699;color:var(--x)}"
        );
    }

    /// `!important` is tightened (`red !important` → `red!important`).
    #[test]
    fn ng_byte_important_tightened() {
        assert_eq!(
            opt(".box.urgent[_ngcontent-%COMP%] { color: red !important; }"),
            ".box.urgent[_ngcontent-%COMP%]{color:red!important}"
        );
    }

    /// An attribute-selector identifier value drops its quotes (`[data-state="open"]` →
    /// `[data-state=open]`).
    #[test]
    fn ng_byte_attr_quotes_stripped() {
        assert_eq!(
            opt(r#".box[data-state="open"][_ngcontent-%COMP%] { display: block; }"#),
            ".box[data-state=open][_ngcontent-%COMP%]{display:block}"
        );
    }

    /// A legacy `::before` pseudo-element collapses to `:before`.
    #[test]
    fn ng_byte_legacy_pseudo_element_collapsed() {
        assert_eq!(
            opt(".box[_ngcontent-%COMP%]::before { content: ''; display: inline-block; }"),
            ".box[_ngcontent-%COMP%]:before{content:\"\";display:inline-block}"
        );
    }

    /// `::selection` (a MODERN pseudo-element) keeps its `::` — only the four legacy CSS2
    /// pseudo-elements collapse.
    #[test]
    fn modern_pseudo_element_keeps_double_colon() {
        assert_eq!(
            opt(".a[_ngcontent-%COMP%]::selection { color: red; }"),
            ".a[_ngcontent-%COMP%]::selection{color:red}"
        );
    }
}
