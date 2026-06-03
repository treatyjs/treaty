//! CSS value optimization toward esbuild — the `minify: true` pass ng-packagr
//! runs over component styles before they are emitted.
//!
//! ## Why this exists
//!
//! ng-packagr hands every component stylesheet (inline `styles: ['…']` and
//! resolved `styleUrls`) to **esbuild** with `minify: true` (see
//! `ng-packagr/src/lib/styles/stylesheets/bundle-options.js`) before Angular folds
//! the result into `ɵɵdefineComponent({ styles: [...] })` and scopes it with the
//! `_ngcontent-%COMP%` emulated-encapsulation placeholders. The placement is:
//!
//! ```text
//!   SCSS/Less/CSS  ──preprocess──▶  plain CSS  ──esbuild minify──▶  ngc scope
//! ```
//!
//! packagr already reproduces the preprocess step ([`crate::stylesheet`]) and the
//! `_ngcontent-%COMP%` scoping (treaty_ivy's `ShadowCss` port). What was missing is
//! the esbuild **value** minification — `color: blue` → `#00f`, `font-weight: bold`
//! → `700`, `#FFFFFF` → `#fff`, `0px` → `0`, `0.5` → `.5`, whitespace collapse — so
//! the emitted `styles` *values* (not just the scoping) match ng-packagr's.
//!
//! ## Order — why packagr optimizes the SCOPED css
//!
//! esbuild minifies *before* Angular scopes, but esbuild's value transforms operate
//! on declaration VALUES while scoping only rewrites SELECTORS (appending
//! `[_ngcontent-%COMP%]` / `[_nghost-%COMP%]`). The two are orthogonal: running the
//! same value transforms over the already-scoped CSS yields byte-identical values,
//! and the placeholders survive untouched (verified against esbuild directly). So
//! packagr runs this optimizer as a post-process over each emitted `styles: [...]`
//! string — which uniformly covers BOTH inline `styles` and resolved `styleUrls`
//! without reaching into treaty_ivy's scoping.
//!
//! ## Fidelity — what matches esbuild and what does not
//!
//! esbuild parses CSS into a typed, per-property value AST: it knows which property
//! *positions* are `<color>` and converts color names/`rgb()`/`hsl()` only there
//! (e.g. `border-color: blue` → `#00f`, but `border: 1px solid blue` is left
//! verbatim because the color sits inside a shorthand it does not re-serialize that
//! way). Reproducing that full typed model is a CSS engine in its own right.
//!
//! This optimizer instead implements the transforms that are **context-free** —
//! correct regardless of which property they appear in, exactly as esbuild would do
//! them — and is conservative everywhere else:
//!
//!   * whitespace collapse + selector/decl punctuation tightening (always);
//!   * hex color normalization `#AABBCC` → `#abc`, `#FFFFFF` → `#fff`, 8→4 digit
//!     shortening, lowercasing (a hex token means the same in any position);
//!   * numeric normalization: drop a leading integer zero (`0.5`→`.5`), trailing
//!     fractional zeros (`0.50`→`.5`, `10.0`→`10`), and the unit on a zero *length*
//!     (`0px`→`0`, `0em`→`0`) while keeping a zero percentage/time (`0%`, `0s`);
//!   * `font-weight` keyword → number (`bold`→`700`, `normal`→`400`) — applied only
//!     to the `font-weight` longhand;
//!   * color *name* → hex (`blue`→`#00f`) — applied only to the **color-only
//!     longhand** properties (`color`, `background-color`, `border-color`, …) whose
//!     value is a single color token, the positions where esbuild always converts.
//!
//! Color names inside shorthands (`border`, `background`, `outline`, `box-shadow`)
//! and `rgb()`/`hsl()` function conversions are deliberately NOT performed — that is
//! where esbuild's typed model diverges from a context-free pass, and doing them
//! blindly would *over*-minify relative to esbuild. The honest result: value-level
//! output is byte-equal to esbuild for the common single-color/whitespace/number/
//! hex cases, and conservatively unchanged (never wrong) for shorthand colors and
//! functional color notations.

/// Optimize one CSS string toward esbuild's `minify: true` output.
///
/// Preserves Angular's `_ngcontent-%COMP%` / `_nghost-%COMP%` scoping placeholders
/// (they live in selectors, which this pass only whitespace-tightens). Returns the
/// minified CSS WITHOUT a trailing newline (the caller decides framing); esbuild
/// appends a trailing `\n`, but the value folded into a `styles: [...]` literal is
/// compared without it here for composability.
pub fn optimize(css: &str) -> String {
    let tokens = tokenize(css);
    let mut out = String::with_capacity(css.len());
    let mut i = 0;
    // Track whether we are inside a declaration block (between `{` and `}`) so we
    // only treat `:` as a declaration separator there (selectors can carry `:` in
    // pseudo-classes, which must not be split into property/value).
    let mut depth: i32 = 0;
    while i < tokens.len() {
        match &tokens[i] {
            Tok::Punct('{') => {
                depth += 1;
                out.push('{');
                i += 1;
            }
            Tok::Punct('}') => {
                depth -= 1;
                // esbuild drops the last declaration's terminating semicolon before
                // a block close (`color:#00f;}` → `color:#00f}`).
                if out.ends_with(';') {
                    out.pop();
                }
                out.push('}');
                i += 1;
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
            _ => {
                // Selector / at-rule prelude text — emit tightened.
                out.push_str(&render_selector_token(&tokens[i]));
                i += 1;
            }
        }
    }
    out
}

/// Whether the whitespace token at `i` is a significant descendant combinator —
/// i.e. it has a non-space, non-`{`/`}`/`;`/`,` token on BOTH sides. Whitespace
/// adjacent to structural punctuation (block braces, separators) is dropped; a
/// space between two compound selectors (`.a .b`) is kept.
fn space_is_significant(tokens: &[Tok], i: usize) -> bool {
    let prev = prev_significant(tokens, i);
    let next = next_significant(tokens, i);
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
    let prop = prop_raw.to_ascii_lowercase();
    let mut j = i + 1;
    // skip spaces + the colon
    while j < tokens.len() && matches!(tokens[j], Tok::Space) {
        j += 1;
    }
    // colon
    if j < tokens.len() && matches!(tokens[j], Tok::Punct(':')) {
        j += 1;
    }
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

    let value = render_value(&prop, &value_tokens);
    let rendered = format!("{prop}:{value};");
    (rendered, j)
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

    // Whether this declaration has exactly one value token (a sole color/keyword),
    // which is the only position where we apply name→hex / keyword→number, matching
    // esbuild's "convert only in a color-typed single position" behaviour.
    let single = pieces.len() == 1;

    let mut out_pieces: Vec<String> = Vec::with_capacity(pieces.len());
    for piece in &pieces {
        out_pieces.push(transform_value_token(prop, piece, single));
    }

    // Join with single spaces, but no space before/after a comma.
    let mut s = String::new();
    for (idx, p) in out_pieces.iter().enumerate() {
        if idx > 0 {
            let prev_comma = out_pieces[idx - 1] == ",";
            let this_comma = p == ",";
            if !prev_comma && !this_comma {
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
        // Matches esbuild's value minification on the color-only / number cases.
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
        // `red` is shorter than `#f00`, so esbuild keeps the name.
        assert!(out.contains("color:red"), "red should stay a name: {out}");
    }

    #[test]
    fn matches_esbuild_scoped_box_exactly() {
        // The exact scoped CSS treaty_ivy emits for the styled-box fixture.
        let input = ".box[_ngcontent-%COMP%] {\n      color: blue;\n      font-weight: bold;\n      background: #ffffff;\n      margin: 0px;\n      padding: 0.500em;\n      border: 1px solid rgb(170, 187, 204);\n    }";
        let out = opt(input);
        // esbuild keeps the color inside the `border` shorthand AND keeps rgb()
        // there verbatim (it does not re-serialize the shorthand). We match that:
        // border value stays `1px solid rgb(170,187,204)` (whitespace tightened).
        assert_eq!(
            out,
            ".box[_ngcontent-%COMP%]{color:#00f;font-weight:700;background:#fff;margin:0;padding:.5em;border:1px solid rgb(170,187,204)}"
        );
    }

    #[test]
    fn keeps_color_inside_border_shorthand_verbatim() {
        // esbuild does NOT convert a color name inside the `border` shorthand.
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
}
