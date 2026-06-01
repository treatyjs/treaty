use crate::treaty::token::{Token, TokenKind};
use std::str::Chars;

use super::token::{ControlFlowKind, DeferKind};

/// Returns the largest byte index `<= index` that lies on a UTF-8 char boundary in `s` (or
/// `s.len()` when `index` is past the end). This is a stable-Rust stand-in for the unstable
/// `str::floor_char_boundary`, used to keep every source slice in the lexer panic-free even if a
/// byte offset were ever computed off a char boundary.
fn floor_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Decides whether a `/` at the cursor begins a REGEX literal (vs. a division operator), given the
/// last significant code char before it.
///
/// A `/` is a regex start UNLESS the previous token can end an expression — i.e. an identifier /
/// keyword char, a digit, or a closing `)` / `]` / `}` (after which `/` is division) — or a closing
/// quote (after a string, `/` is division). At the start of a region (`None`) a `/` is a regex. This
/// is intentionally conservative: when the previous char ends an expression we treat `/` as divide
/// (the prior behavior), so this only ADDS regex coverage and never reclassifies real division.
///
/// Note this is deliberately distinct from `plugin::can_end_statement` (which governs ASI / `server`
/// block detection): a digit and a `}` both END an expression for regex-vs-divide purposes (`a[0]/b`,
/// `obj{}/b`), whereas ASI treats a bare `}` as a statement terminator. Keeping the rule local avoids
/// coupling two different lexical questions.
fn regex_allowed_after(last_significant: Option<char>) -> bool {
    match last_significant {
        // Start of region: a `/` here can only be a regex (or a comment, handled earlier).
        None => true,
        Some(c) => !(c.is_alphanumeric() || c == '_' || c == '$' || matches!(c, ')' | ']' | '}' | '\'' | '"' | '`')),
    }
}

/// The HTML void elements: elements that are always empty and have no end tag, so an authored
/// `<br>` / `<input>` (with or without a trailing `/`) opens AND closes in one tag. Matched
/// ASCII-case-insensitively (HTML tag names are case-insensitive). Source: the WHATWG HTML "void
/// elements" list.
const VOID_ELEMENTS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// True if `tag_name` is an HTML void element (ASCII-case-insensitive), e.g. `br`, `BR`, `Input`.
fn is_void_element(tag_name: &str) -> bool {
    VOID_ELEMENTS
        .iter()
        .any(|v| v.eq_ignore_ascii_case(tag_name))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexerState {
    Default,
    JavaScript,
    HTML,
    CSS,
    TemplateExpression,
    ControlFlow,
    Macro,
}

pub struct Lexer<'a> {
    input: &'a str,
    chars: Chars<'a>,
    pos: usize,
    current_char: Option<char>,
    state: LexerState,
    state_stack: Vec<LexerState>, // Stack to keep track of parent states
    /// True until the first content token is produced. A ```-fenced macro block is only
    /// recognized while this holds (the macro block must be at the TOP of the file).
    at_file_top: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        let mut chars = input.chars();
        let current_char = chars.next();
        Lexer {
            input,
            chars,
            pos: 0,
            current_char,
            state: LexerState::Default,
            state_stack: Vec::new(), // Initialize the state stack
            at_file_top: true,
        }
    }

    pub fn next_token(&mut self) -> Option<Token> {
        self.consume_whitespace();

        match self.state {
            LexerState::Default => self.lex_default_state(),
            LexerState::JavaScript => self.parse_javascript(),
            LexerState::HTML => self.parse_html(),
            LexerState::CSS => self.parse_style(),
            LexerState::TemplateExpression => self.parse_template_expression(),
            LexerState::ControlFlow => self.parse_control_flow(),
            LexerState::Macro => self.parse_macro(),
        }
    }

    /// Advances the lexer by one character.
    fn advance(&mut self) {
        self.pos += self.current_char.unwrap_or('\0').len_utf8();
        self.current_char = self.chars.next();
    }

    /// Consumes characters while the condition is true.
    fn consume_while<F>(&mut self, mut condition: F) -> String
    where
        F: FnMut(char) -> bool,
    {
        let mut result = String::new();
        while let Some(ch) = self.current_char {
            if condition(ch) {
                result.push(ch);
                self.advance();
            } else {
                break;
            }
        }
        result
    }

    /// Consumes whitespace characters.
    fn consume_whitespace(&mut self) {
        self.consume_while(|ch| ch.is_whitespace());
    }

    /// Retrieves the next token.
    fn lex_default_state(&mut self) -> Option<Token> {
        let current_char = self.current_char?;

        // A ```-fenced block at the TOP of the file is a compile-time macro block. This is the only
        // place a macro is recognized; `at_file_top` is cleared once any other token is produced.
        if self.at_file_top && current_char == '`' && self.starts_with("```") {
            self.at_file_top = false;
            self.push_state(LexerState::Macro);
            return self.parse_macro();
        }
        self.at_file_top = false;

        match current_char {
            '<' if self.starts_with_style_open() => {
                self.push_state(LexerState::CSS);
                self.parse_style()
            }
            // A `<template>…</template>` wrapper is OPTIONAL per the `.treaty` spec: HTML is detected
            // by its tags directly and TS/HTML/<style> freely interleave. When an author *does* wrap
            // their markup in `<template>` (the migration-friendly form), the wrapper itself is not
            // part of the rendered template — only its CONTENTS are. So unwrap it: lex the inner
            // markup as the HTML region and drop the surrounding `<template>`/`</template>` tags.
            '<' if self.starts_with_template_open() => {
                self.push_state(LexerState::HTML);
                self.parse_template_wrapper()
            }
            '<' => {
                self.push_state(LexerState::HTML);
                self.parse_html()
            }
            '{' if self.starts_with("{{") => {
                self.advance_by(2); // Skip '{{'
                self.push_state(LexerState::TemplateExpression);
                self.parse_template_expression()
            }
            '@' => self.parse_control_flow(),
            _ => {
                self.push_state(LexerState::JavaScript);
                self.parse_javascript()
            }
        }
    }

    /// True if the input at the cursor opens a `<style` tag (with or without attributes), i.e.
    /// `<style>` or `<style ...>`. Used so that `<style lang="scss">` is still recognized as CSS.
    fn starts_with_style_open(&self) -> bool {
        let rest = self.rest();
        if let Some(after) = rest.strip_prefix("<style") {
            // The next char must be whitespace, `>`, or `/` — otherwise it's e.g. `<styled>`.
            matches!(after.chars().next(), Some(c) if c.is_whitespace() || c == '>' || c == '/')
                || after.is_empty()
        } else {
            false
        }
    }

    /// True if the input at the cursor opens a `<template` tag (with or without attributes), i.e.
    /// `<template>` or `<template …>`. The next char after `template` must be whitespace, `>`, or
    /// `/` so that e.g. `<templated>` is NOT mistaken for the optional template wrapper.
    fn starts_with_template_open(&self) -> bool {
        let rest = self.rest();
        if let Some(after) = rest.strip_prefix("<template") {
            matches!(after.chars().next(), Some(c) if c.is_whitespace() || c == '>' || c == '/')
                || after.is_empty()
        } else {
            false
        }
    }

    /// Parses an OPTIONAL `<template …>…</template>` wrapper, emitting only its INNER markup as the
    /// HTML region (the wrapper tags are dropped). The cursor is positioned at the opening
    /// `<template`. Nested `<template>` elements are balanced so the region ends at the matching
    /// outer `</template>`. All cursor motion goes through [`Self::advance`], so byte offsets stay on
    /// UTF-8 char boundaries even when the inner markup contains non-ASCII text.
    fn parse_template_wrapper(&mut self) -> Option<Token> {
        // Consume the opening `<template …>` tag (and any attributes), stopping after its `>`.
        self.advance_by_str("<template");
        self.consume_until('>');
        if self.current_char == Some('>') {
            self.advance(); // Skip '>'
        }

        let start_pos = self.pos;
        let mut content_end = self.pos;
        let mut depth = 1usize;

        while let Some(ch) = self.current_char {
            if ch == '<' {
                if self.starts_with("</template>") {
                    depth -= 1;
                    if depth == 0 {
                        // End of the wrapper: the inner markup is everything up to here.
                        content_end = self.pos;
                        self.advance_by_str("</template>");
                        break;
                    }
                    self.advance_by_str("</template>");
                    content_end = self.pos;
                    continue;
                }
                if self.starts_with_template_open() {
                    depth += 1;
                    self.advance_by_str("<template");
                    content_end = self.pos;
                    continue;
                }
            }
            self.advance();
            content_end = self.pos;
        }

        let value = self.slice(start_pos, content_end);
        self.pop_state(); // Return to the previous state
        Some(Token::new(TokenKind::HTML(value), start_pos, content_end))
    }

    /// Parses a JavaScript/TypeScript block with a balanced, lexical-state-aware scanner.
    ///
    /// The old scanner ended a JS region at the first raw newline / `;`, the first `<` that opened a
    /// `</`/`<style`, the first `{{`, or the first `@` — with NO tracking of strings, comments, regex
    /// literals, or bracket nesting. That split a region MID-EXPRESSION whenever any of those bytes
    /// appeared inside a string, a comment, a regex body, or a balanced `()`/`[]`/`{}` group: a
    /// multi-line object literal, a `computed(() => { … })` arrow, a regex `/[<{;]/`, or a string
    /// `"</template>"` would all be torn apart, shoving the tail of the expression into an
    /// HTML/interpolation/control-flow token and corrupting the JS region.
    ///
    /// This scanner instead tracks lexical state — string literals (`'`/`"`/`` ` `` with `\`-escapes
    /// and `${ … }` template-substitution nesting), line + block comments, REGEX literals
    /// (disambiguated from division by the last significant code char), and `()`/`[]`/`{}` DEPTH — and
    /// only treats a region boundary as REAL when the cursor is at top level (depth 0) and outside any
    /// string/comment/regex. A boundary inside any of those is ordinary JS content and is consumed as
    /// code. The set of recognized top-level boundaries is unchanged (a real top-level newline / `;`,
    /// a `</`/`<style` open, a `{{` interpolation, or an `@` control-flow marker), so well-formed TS
    /// lexes into the same region stream as before — only the false-positive splits are removed.
    fn parse_javascript(&mut self) -> Option<Token> {
        let start_pos = self.pos;
        // Bracket nesting across `()`, `[]`, and `{}`. A region boundary only counts at depth 0.
        let mut depth: i32 = 0;
        // The last significant (non-whitespace, non-comment) code char seen, used to disambiguate a
        // `/` as the start of a regex literal vs. a division operator.
        let mut last_significant: Option<char> = None;

        while let Some(ch) = self.current_char {
            match ch {
                // String / template literals: consume the whole literal (escapes + `${}` nesting for
                // template strings) so a `<`, `{{`, `;`, newline, or `@` inside it is never a boundary.
                '\'' | '"' | '`' => {
                    self.consume_string(ch);
                    last_significant = Some(ch);
                }

                // Comments and regex literals both begin with `/`.
                '/' => {
                    if self.starts_with("//") {
                        self.consume_line_comment();
                        // A line comment is insignificant; `last_significant` is unchanged.
                    } else if self.starts_with("/*") {
                        self.consume_block_comment();
                        // A block comment is insignificant; `last_significant` is unchanged.
                    } else if regex_allowed_after(last_significant) {
                        // `/` in regex position: consume a full regex literal (body honoring
                        // `\`-escapes and `[ … ]` character classes, then flags). A `<`/`{{`/`;`/`@`
                        // inside the body is regex content, never a region boundary.
                        self.consume_regex_literal();
                        last_significant = Some('/');
                    } else {
                        // Division operator.
                        self.advance();
                        last_significant = Some('/');
                    }
                }

                // ── Region boundaries (only honored at top level / depth 0) ────────────────────────
                // These arms precede the bracket-nesting arms so that a top-level `{{` is recognized
                // as an interpolation boundary before the generic `{` depth-opener matches it.
                '\n' | '\r' | '\u{000C}' | ';' if depth == 0 => {
                    self.advance();
                    break;
                }
                '<' if depth == 0 && (self.starts_with_style_open() || self.starts_with("</")) => {
                    break
                }
                '{' if depth == 0 && self.starts_with("{{") => break,
                '@' if depth == 0 && self.pos > start_pos => {
                    // A control-flow marker at top level ends the JS region. Only break when there is
                    // real JS before it (`self.pos > start_pos`) so a leading `@` is never an empty
                    // token; the default-state router opens control flow at a leading `@`.
                    self.push_state(LexerState::ControlFlow);
                    break;
                }

                // Bracket nesting. A `{` that reaches here is a real code brace (a top-level `{{` was
                // already handled by the boundary arm above).
                '(' | '[' | '{' => {
                    depth += 1;
                    self.advance();
                    last_significant = Some(ch);
                }
                ')' | ']' | '}' => {
                    if depth > 0 {
                        depth -= 1;
                    }
                    self.advance();
                    last_significant = Some(ch);
                }

                // Inside a bracket group (depth > 0) these are ordinary code, not boundaries — the
                // depth-0 boundary arms above did not fire, so consume them as code. (`{`/`}`/`<`/`@`
                // are handled by the arms above/below; this arm covers the statement separators.)
                '\n' | '\r' | '\u{000C}' | ';' => {
                    self.advance();
                    // Whitespace is insignificant; only `;` updates `last_significant`.
                    if ch == ';' {
                        last_significant = Some(';');
                    }
                }

                // Any other code char.
                _ => {
                    self.advance();
                    if !ch.is_whitespace() {
                        last_significant = Some(ch);
                    }
                }
            }
        }

        let end_pos = self.pos;
        let value = self.slice(start_pos, end_pos);
        self.pop_state(); // Return to the previous state
        Some(Token::new(TokenKind::JavaScript(value), start_pos, end_pos))
    }

    /// Consumes a regex literal whose opening `/` is at the cursor. The body honors `\`-escapes and
    /// `[ … ]` character classes (a `/` inside a class is literal, not the terminator), then the
    /// trailing flag identifier characters. Defensive against an unterminated literal (stops at EOL /
    /// EOF) so a stray `/` can never spin the scanner.
    fn consume_regex_literal(&mut self) {
        self.advance(); // Skip the opening `/`.
        let mut in_class = false;
        while let Some(ch) = self.current_char {
            match ch {
                '\\' => {
                    self.advance(); // Skip the backslash.
                    if self.current_char.is_some() {
                        self.advance(); // Skip the escaped char.
                    }
                }
                '[' => {
                    in_class = true;
                    self.advance();
                }
                ']' if in_class => {
                    in_class = false;
                    self.advance();
                }
                '/' if !in_class => {
                    self.advance(); // Skip the closing `/`.
                    break;
                }
                // A raw newline cannot appear in a regex literal: bail rather than run away.
                '\n' | '\r' => break,
                _ => self.advance(),
            }
        }
        // Consume regex flags (e.g. `gimsuy`).
        self.consume_while(|c| c.is_ascii_alphabetic());
    }

    /// Parses a `<style …>…</style>` block.
    ///
    /// The cursor is positioned at the opening `<style`. The opening tag (and any attributes) is
    /// consumed first, capturing an optional `lang="…"` preprocessor language; the CSS body then
    /// runs up to the closing `</style>`.
    fn parse_style(&mut self) -> Option<Token> {
        // Consume the opening `<style …>` tag and capture the `lang` attribute, if present.
        debug_assert!(self.starts_with("<style"));
        self.advance_by_str("<style");
        let lang = self.consume_style_open_tag();

        let start_pos = self.pos;

        while let Some(ch) = self.current_char {
            if self.starts_with("</style>") {
                break;
            }
            match ch {
                '/' if self.starts_with("/*") => self.consume_block_comment(),
                '{' => self.advance(), // Advance over '{', you might want to handle nested blocks
                '}' => self.advance(), // Advance over '}', matching any opened '{'
                _ => self.advance(),
            }
        }

        let end_pos = self.pos;
        let value = self.slice(start_pos, end_pos);

        if self.starts_with("</style>") {
            self.advance_by_str("</style>");
        }

        self.pop_state(); // Return to the previous state
        Some(Token::new(
            TokenKind::Style { content: value, lang },
            start_pos,
            end_pos,
        ))
    }

    /// Consumes the rest of a `<style …>` opening tag (the cursor sits just after `<style`),
    /// stopping after the closing `>`. Returns the value of a `lang="…"`/`lang='…'` attribute if
    /// one is present.
    fn consume_style_open_tag(&mut self) -> Option<String> {
        let mut lang: Option<String> = None;
        while let Some(ch) = self.current_char {
            match ch {
                '>' => {
                    self.advance(); // Skip '>'
                    break;
                }
                // Capture `lang="scss"` / `lang='sass'` (with optional whitespace around `=`).
                'l' | 'L' if self.starts_with_ignore_ascii_case("lang") => {
                    self.advance_by_str("lang");
                    self.consume_whitespace();
                    if self.current_char == Some('=') {
                        self.advance(); // Skip '='
                        self.consume_whitespace();
                        if let Some(q @ ('"' | '\'')) = self.current_char {
                            self.advance(); // Skip opening quote
                            let value = self.consume_while(|c| c != q);
                            if self.current_char == Some(q) {
                                self.advance(); // Skip closing quote
                            }
                            let trimmed = value.trim();
                            if !trimmed.is_empty() {
                                lang = Some(trimmed.to_string());
                            }
                        }
                    }
                }
                '\'' | '"' | '`' => self.consume_string(ch),
                _ => self.advance(),
            }
        }
        lang
    }

    /// Parses a ```-fenced macro block at the top of the file.
    ///
    /// The cursor is positioned at the opening ```` ``` ````. The opening fence's info string
    /// (anything on the rest of that line, e.g. ```` ```rsc ````) is captured as `info`; the raw
    /// body up to the closing ```` ``` ```` fence is captured as `content`.
    fn parse_macro(&mut self) -> Option<Token> {
        let token_start = self.pos;
        self.advance_by_str("```"); // Skip the opening fence

        // The remainder of the opening line is the optional info string.
        let info_raw = self.consume_while(|c| c != '\n' && c != '\r');
        let info_trimmed = info_raw.trim();
        let info = if info_trimmed.is_empty() {
            None
        } else {
            Some(info_trimmed.to_string())
        };
        // Skip the newline terminating the opening fence line.
        if self.current_char == Some('\r') {
            self.advance();
        }
        if self.current_char == Some('\n') {
            self.advance();
        }

        let content_start = self.pos;
        while self.current_char.is_some() {
            if self.starts_with("```") {
                break;
            }
            self.advance();
        }
        let content = self.slice(content_start, self.pos);

        if self.starts_with("```") {
            self.advance_by_str("```");
        }

        self.pop_state(); // Return to the previous state
        Some(Token::new(
            TokenKind::Macro { content, info },
            token_start,
            self.pos,
        ))
    }

    /// Parses an HTML segment.
    ///
    /// The element stack is what keeps the HTML region open until its root element closes. Two
    /// rough edges from the lexer audit are handled here:
    ///
    /// - VOID elements (`<br>`, `<input>`, `<img>`, `<hr>`, `<meta>`, … — the full HTML void set) have
    ///   no end tag. The old scanner pushed EVERY opening tag and only popped on `/>` or `</tag>`, so a
    ///   bare `<br>` / `<input>` (no trailing slash) was pushed and never popped — its element stack
    ///   never emptied and the HTML region swallowed the rest of the file (including the trailing TS).
    ///   A void element is now treated as self-closing: it is never pushed.
    /// - `{{ … }}` interpolation is consumed WHOLE (string-aware, brace-balanced) so a `<` inside an
    ///   interpolation expression (`{{ a < b }}`) is interpolation text, not a tag open — the old
    ///   scanner saw that `<` as a new element and corrupted the region.
    fn parse_html(&mut self) -> Option<Token> {
        let start_pos = self.pos;
        let mut tag_stack: Vec<String> = Vec::new();

        while self.current_char.is_some() {
            self.consume_whitespace();
            // Re-read the current character after consuming whitespace; it must not be the value
            // captured at the top of the loop (that goes stale once whitespace is skipped).
            let Some(ch) = self.current_char else { break };
            if ch == '<' {
                if self.starts_with("<!--") {
                    self.consume_html_comment();
                } else if self.starts_with("</") {
                    self.advance_by(2); // Skip '</'
                    let tag_name = self.consume_tag_name();
                    let expected_tag = tag_stack.pop();
                    if let Some(expected_tag) = &expected_tag {
                        if &tag_name != expected_tag {
                            // Handle mismatched tag (optional)
                        }
                    }
                    self.consume_until('>'); // Skip until '>'
                    self.advance(); // Skip '>'
                    if tag_stack.is_empty() {
                        break;
                    }
                } else {
                    self.advance(); // Skip '<'
                    let tag_name = self.consume_tag_name();
                    // A void element (`<br>`, `<input>`, …) has no end tag; do not push it, or the
                    // element stack never empties and the region runs to EOF. Consume its attributes
                    // either way so a quoted `>` inside an attribute does not end the tag early.
                    let is_void = is_void_element(&tag_name);
                    let self_closing = self.consume_attributes();
                    if is_void || self_closing {
                        // The tag opens and closes immediately. If it is the root, the region is done.
                        if tag_stack.is_empty() {
                            break;
                        }
                    } else {
                        tag_stack.push(tag_name);
                    }
                }
            } else if ch == '{' && self.starts_with("{{") {
                // `{{ … }}` interpolation stays INSIDE the HTML token (the established contract); a
                // `<` inside it is interpolation text, not a tag. Consume the whole interpolation.
                self.consume_interpolation();
            } else {
                self.advance();
            }
        }

        let end_pos = self.pos;
        let value = self.slice(start_pos, end_pos);
        self.pop_state(); // Return to the previous state
        Some(Token::new(TokenKind::HTML(value), start_pos, end_pos))
    }

    /// Consumes a `{{ … }}` interpolation starting at the opening `{{`, stopping just past the
    /// matching `}}`. Strings inside the expression are consumed whole, so a `}}` or `<` inside a
    /// string literal is not mistaken for the terminator / a tag. Brace nesting (object literals) is
    /// balanced. Defensive against an unterminated interpolation (stops at EOF).
    fn consume_interpolation(&mut self) {
        self.advance_by(2); // Skip the opening `{{`.
        let mut brace_depth: i32 = 0;
        while let Some(ch) = self.current_char {
            match ch {
                '\'' | '"' | '`' => self.consume_string(ch),
                '}' if brace_depth == 0 && self.starts_with("}}") => {
                    self.advance_by(2); // Skip the closing `}}`.
                    break;
                }
                '{' => {
                    brace_depth += 1;
                    self.advance();
                }
                '}' => {
                    if brace_depth > 0 {
                        brace_depth -= 1;
                    }
                    self.advance();
                }
                _ => self.advance(),
            }
        }
    }

    /// Parses a template expression.
    fn parse_template_expression(&mut self) -> Option<Token> {
        let start_pos = self.pos;
        let mut brace_count = 0;

        while let Some(ch) = self.current_char {
            if ch == '{' {
                brace_count += 1;
            } else if ch == '}' {
                brace_count -= 1;
                if brace_count == -2 {
                    // We've found the closing '}}'
                    break;
                }
            } else if ch == '\'' || ch == '"' || ch == '`' {
                self.consume_string(ch);
                continue;
            }
            self.advance();
        }

        if self.starts_with("}}") {
            self.advance_by(2); // Skip '}}'
        }

        let end_pos = self.pos;
        let value = self.slice(start_pos, end_pos);
        self.pop_state(); // Return to the previous state
        Some(Token::new(
            TokenKind::TemplateExpression(value.trim().to_string()),
            start_pos,
            end_pos,
        ))
    }

    /// Parses control flow statements (@if, @for, etc.).
    fn parse_control_flow(&mut self) -> Option<Token> {
        let start_pos = self.pos;

        if self.starts_with("@if") {
            self.advance_by_str("@if");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::If), start_pos, self.pos));
        } else if self.starts_with("@else if") {
            self.advance_by_str("@else if");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::ElseIf), start_pos, self.pos));
        } else if self.starts_with("@else") {
            self.advance_by_str("@else");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Else), start_pos, self.pos));
        } else if self.starts_with("@for") {
            self.advance_by_str("@for");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::For), start_pos, self.pos));
        } else if self.starts_with("@empty") {
            self.advance_by_str("@empty");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Empty), start_pos, self.pos));
        } else if self.starts_with("@switch") {
            self.advance_by_str("@switch");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Switch), start_pos, self.pos));
        } else if self.starts_with("@case") {
            self.advance_by_str("@case");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Case), start_pos, self.pos));
        } else if self.starts_with("@default") {
            self.advance_by_str("@default");
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Default), start_pos, self.pos));
        } else if self.starts_with("@defer") {
            self.advance_by_str("@defer");
            return Some(Token::new(TokenKind::Defer(DeferKind::Defer), start_pos, self.pos));
        } else if self.starts_with("@placeholder") {
            self.advance_by_str("@placeholder");
            return Some(Token::new(TokenKind::Defer(DeferKind::Placeholder), start_pos, self.pos));
        } else if self.starts_with("@loading") {
            self.advance_by_str("@loading");
            return Some(Token::new(TokenKind::Defer(DeferKind::Loading), start_pos, self.pos));
        } else if self.starts_with("@error") {
            self.advance_by_str("@error");
            return Some(Token::new(TokenKind::Defer(DeferKind::Error), start_pos, self.pos));
        } else {
            // If not a recognized control flow, assume it's JavaScript
            self.state = LexerState::JavaScript;
            self.advance(); // Ensure we advance the position to avoid infinite loop
            self.parse_javascript()
        }
    }

    /// The remaining input from the cursor. `self.pos` is always a UTF-8 char boundary (every
    /// [`Self::advance`] moves it by a whole `char`'s `len_utf8()`), so this slice never panics.
    fn rest(&self) -> &'a str {
        &self.input[self.pos..]
    }

    /// A char-boundary-safe slice of the source. `start`/`end` are byte offsets produced by the
    /// cursor (hence already on char boundaries); the bounds are floored/ordered defensively so a
    /// stray offset can never split a multi-byte char and panic.
    fn slice(&self, start: usize, end: usize) -> String {
        let lo = floor_char_boundary(self.input, start.min(end));
        let hi = floor_char_boundary(self.input, end.max(start));
        self.input[lo..hi].to_string()
    }

    /// Checks if the upcoming characters match the given string.
    fn starts_with(&self, s: &str) -> bool {
        self.rest().starts_with(s)
    }

    /// Like [`Self::starts_with`] but ASCII-case-insensitive (used for attribute names).
    fn starts_with_ignore_ascii_case(&self, s: &str) -> bool {
        let rest = self.rest();
        rest.len() >= s.len() && rest.as_bytes()[..s.len()].eq_ignore_ascii_case(s.as_bytes())
    }

    /// Advances the lexer past exactly the literal `s`, by characters (not bytes), keeping the
    /// cursor on a UTF-8 char boundary. The caller must have verified the cursor is at `s` (e.g. via
    /// [`Self::starts_with`]); this is the safe replacement for `advance_by(s.len())`, which would
    /// over-advance when `s` is multi-byte.
    fn advance_by_str(&mut self, s: &str) {
        for _ in s.chars() {
            self.advance();
        }
    }

    /// Advances the lexer by a given number of characters.
    fn advance_by(&mut self, n: usize) {
        for _ in 0..n {
            self.advance();
        }
    }

    /// Consumes a string literal, handling escaped characters. A backtick opens a TEMPLATE literal,
    /// which is delegated to [`Self::consume_template_literal`] so its `${ … }` substitutions (which
    /// may themselves contain `` ` ``, `'`, `"`, and balanced braces) are balanced correctly rather
    /// than terminating at the first stray backtick.
    fn consume_string(&mut self, delimiter: char) {
        if delimiter == '`' {
            self.consume_template_literal();
            return;
        }
        self.advance(); // Skip the opening quote
        while let Some(ch) = self.current_char {
            match ch {
                '\\' => {
                    self.advance(); // Skip the backslash
                    self.advance(); // Skip the escaped character
                }
                ch if ch == delimiter => {
                    self.advance(); // Skip the closing quote
                    break;
                }
                _ => self.advance(),
            }
        }
    }

    /// Consumes a template literal (`` `…` ``) whose opening backtick is at the cursor, balancing any
    /// `${ … }` substitution. Inside a substitution the scanner recurses into nested strings /
    /// template literals and tracks `{`/`}` depth, so a `` ` `` or a `}` inside an inner string does
    /// not prematurely close the substitution or the outer template. This is what lets a JS region
    /// hold a multi-line / nested template literal (e.g. `` `Hello, ${ user(`${id}`) }!` ``) intact.
    fn consume_template_literal(&mut self) {
        self.advance(); // Skip the opening backtick.
        while let Some(ch) = self.current_char {
            match ch {
                '\\' => {
                    self.advance(); // Skip the backslash
                    if self.current_char.is_some() {
                        self.advance(); // Skip the escaped char
                    }
                }
                '`' => {
                    self.advance(); // Closing backtick.
                    break;
                }
                '$' if self.peek() == Some('{') => {
                    self.advance(); // Skip '$'
                    self.advance(); // Skip '{'
                    self.consume_template_substitution();
                }
                _ => self.advance(),
            }
        }
    }

    /// Consumes the inside of a `${ … }` template substitution after its opening `{` has been
    /// consumed, stopping just past the matching `}`. Strings and nested template literals inside the
    /// substitution are consumed whole, and `{`/`}` nesting (object literals, blocks) is balanced.
    fn consume_template_substitution(&mut self) {
        let mut depth: i32 = 1;
        while let Some(ch) = self.current_char {
            match ch {
                '\'' | '"' | '`' => self.consume_string(ch),
                '{' => {
                    depth += 1;
                    self.advance();
                }
                '}' => {
                    depth -= 1;
                    self.advance();
                    if depth == 0 {
                        break;
                    }
                }
                _ => self.advance(),
            }
        }
    }

    /// Consumes a line comment.
    fn consume_line_comment(&mut self) {
        while let Some(ch) = self.current_char {
            if ch == '\n' {
                break;
            }
            self.advance();
        }
    }

    /// Consumes a block comment.
    fn consume_block_comment(&mut self) {
        self.advance_by(2); // Skip '/*'
        while self.current_char.is_some() {
            if self.starts_with("*/") {
                self.advance_by(2); // Skip '*/'
                break;
            }
            self.advance();
        }
    }

    /// Consumes an HTML comment.
    fn consume_html_comment(&mut self) {
        self.advance_by_str("<!--");
        while self.current_char.is_some() {
            if self.starts_with("-->") {
                self.advance_by_str("-->");
                break;
            }
            self.advance();
        }
    }

    fn consume_tag_name(&mut self) -> String {
        let mut tag_name = String::new();
        while let Some(ch) = self.current_char {
            if ch.is_alphanumeric() {
                tag_name.push(ch);
                self.advance();
            } else {
                break;
            }
        }
        tag_name
    }

    fn consume_attributes(&mut self) -> bool {
        let mut self_closing = false;
        while let Some(ch) = self.current_char {
            match ch {
                '>' => {
                    self.advance();
                    break;
                }
                '/' if self.peek() == Some('>') => {
                    // Self-closing tag
                    self.advance_by(2); // Skip '/>'
                    self_closing = true;
                    break;
                }
                '\'' | '"' | '`' => self.consume_string(ch),
                _ => self.advance(),
            }
        }
        self_closing
    }

    fn consume_until(&mut self, target: char) {
        while let Some(ch) = self.current_char {
            if ch == target {
                break;
            }
            self.advance();
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.clone().next()
    }

    fn push_state(&mut self, state: LexerState) {
        self.state_stack.push(self.state);
        self.state = state;
    }

    fn pop_state(&mut self) {
        if let Some(state) = self.state_stack.pop() {
            self.state = state;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drains the lexer into the full list of token kinds.
    fn lex(input: &str) -> Vec<TokenKind> {
        let mut lexer = Lexer::new(input);
        let mut kinds = Vec::new();
        while let Some(tok) = lexer.next_token() {
            kinds.push(tok.kind);
        }
        kinds
    }

    #[test]
    fn style_block_captures_lang_attribute() {
        let src = "<style lang=\"scss\">.a { color: red; }</style>";
        let kinds = lex(src);
        let style = kinds
            .iter()
            .find_map(|k| match k {
                TokenKind::Style { content, lang } => Some((content, lang)),
                _ => None,
            })
            .expect("expected a Style token");
        assert_eq!(style.1.as_deref(), Some("scss"));
        assert!(style.0.contains("color: red"), "css body missing; got {:?}", style.0);
    }

    #[test]
    fn style_block_single_quoted_sass_lang() {
        let kinds = lex("<style lang='sass'>.a\n  color: red</style>");
        assert!(
            kinds.iter().any(|k| matches!(
                k,
                TokenKind::Style { lang, .. } if lang.as_deref() == Some("sass")
            )),
            "expected Style with lang=sass; got {:?}",
            kinds
        );
    }

    #[test]
    fn plain_style_block_has_no_lang() {
        let kinds = lex("<style>.a { color: red; }</style>");
        let lang = kinds
            .iter()
            .find_map(|k| match k {
                TokenKind::Style { lang, .. } => Some(lang.clone()),
                _ => None,
            })
            .expect("expected a Style token");
        assert_eq!(lang, None);
    }

    #[test]
    fn top_of_file_fence_is_a_macro_chunk() {
        let src = "```\nexport const x = 1;\n```\n<div>hi</div>";
        let kinds = lex(src);
        let macro_tok = kinds
            .iter()
            .find_map(|k| match k {
                TokenKind::Macro { content, info } => Some((content.clone(), info.clone())),
                _ => None,
            })
            .expect("expected a Macro token");
        assert_eq!(macro_tok.1, None, "no info string expected");
        assert!(
            macro_tok.0.contains("export const x = 1;"),
            "macro body missing; got {:?}",
            macro_tok.0
        );
        // The HTML that follows the macro is still lexed as HTML.
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::HTML(h) if h.contains("hi"))),
            "expected HTML after macro; got {:?}",
            kinds
        );
    }

    #[test]
    fn macro_fence_captures_info_string() {
        let kinds = lex("```rsc\nlet a = 1;\n```\n");
        assert!(
            kinds.iter().any(|k| matches!(
                k,
                TokenKind::Macro { info, .. } if info.as_deref() == Some("rsc")
            )),
            "expected Macro with info=rsc; got {:?}",
            kinds
        );
    }

    #[test]
    fn fence_only_recognized_at_file_top() {
        // A fence that is NOT at the top of the file is not a macro; it stays JavaScript.
        let kinds = lex("const a = 1;\n```\nnot a macro\n```");
        assert!(
            !kinds.iter().any(|k| matches!(k, TokenKind::Macro { .. })),
            "fence below file top must not be a Macro; got {:?}",
            kinds
        );
    }

    #[test]
    fn ts_outside_tags_is_javascript() {
        // The default: anything not in an HTML tag nor <style> is JavaScript/TypeScript.
        let kinds = lex("const x: number = 1;");
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::JavaScript(js) if js.contains("const x"))),
            "expected JavaScript chunk; got {:?}",
            kinds
        );
        assert!(
            !kinds.iter().any(|k| matches!(
                k,
                TokenKind::HTML(_) | TokenKind::Style { .. } | TokenKind::Macro { .. }
            )),
            "TS-by-default leaked into a non-JS chunk; got {:?}",
            kinds
        );
    }

    #[test]
    fn floor_char_boundary_floors_into_multibyte_char() {
        // "é" is two bytes (0xC3 0xA9); byte index 1 is mid-char and must floor back to 0.
        let s = "é";
        assert_eq!(floor_char_boundary(s, 0), 0);
        assert_eq!(floor_char_boundary(s, 1), 0);
        assert_eq!(floor_char_boundary(s, 2), 2);
        // Past the end clamps to len.
        assert_eq!(floor_char_boundary(s, 99), s.len());
    }

    #[test]
    fn non_ascii_in_every_region_does_not_panic() {
        // FIX #1: non-ASCII text in macro / JS / HTML / <style> / interpolation regions must not
        // trigger a mid-UTF-8-char byte slice. Each region carries an accented word and an emoji.
        let src = "```rsc\nconst gru\u{00df} = 'caf\u{00e9} \u{1F680}';\nreturn { gru\u{00df} };\n```\n\
const t\u{00ed}tulo = 'na\u{00ef}ve \u{1F600}';\n\
<section title=\"caf\u{00e9} \u{1F4A1}\">na\u{00ef}ve \u{1F680} {{ t\u{00ed}tulo }}</section>\n\
more\u{00e9}TS();\n\
<style>/* caf\u{00e9} \u{1F680} */ .a { content: \"\u{00e9}\"; }</style>";
        // The whole token stream must drain without panicking.
        let kinds = lex(src);
        // And the non-ASCII content must survive intact in the captured chunks.
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::Macro { content, .. } if content.contains("caf\u{00e9} \u{1F680}"))),
            "macro lost its non-ASCII content; got {:?}",
            kinds
        );
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::HTML(h) if h.contains("na\u{00ef}ve \u{1F680}"))),
            "HTML lost its non-ASCII content; got {:?}",
            kinds
        );
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::Style { content, .. } if content.contains("\u{00e9}"))),
            "style lost its non-ASCII content; got {:?}",
            kinds
        );
    }

    #[test]
    fn optional_template_wrapper_is_unwrapped() {
        // FIX #2: a `<template>…</template>` wrapper is optional. When present, only its INNER markup
        // is the template region — the wrapper tags are dropped (migration-friendly).
        let src = "const x = 1;\n<template>\n  <div class=\"c\">hi</div>\n</template>\nconst y = 2;";
        let kinds = lex(src);
        let html = kinds
            .iter()
            .find_map(|k| match k {
                TokenKind::HTML(h) => Some(h.clone()),
                _ => None,
            })
            .expect("expected an HTML token");
        assert!(
            html.contains("<div class=\"c\">hi</div>"),
            "inner markup missing; got {:?}",
            html
        );
        assert!(
            !html.contains("<template") && !html.contains("</template>"),
            "wrapper tags leaked into the template region; got {:?}",
            html
        );
        // TS on both sides of the wrapper is preserved as JavaScript.
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::JavaScript(js) if js.contains("const x"))),
            "leading TS lost; got {:?}",
            kinds
        );
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::JavaScript(js) if js.contains("const y"))),
            "trailing TS lost; got {:?}",
            kinds
        );
    }

    #[test]
    fn nested_template_wrapper_balances_to_outer_close() {
        // A nested `<template>` inside the wrapper stays part of the inner markup; the region ends at
        // the matching OUTER `</template>`.
        let src = "<template><div><template>inner</template></div></template>const after = 1;";
        let kinds = lex(src);
        let html = kinds
            .iter()
            .find_map(|k| match k {
                TokenKind::HTML(h) => Some(h.clone()),
                _ => None,
            })
            .expect("expected an HTML token");
        assert!(
            html.contains("<div><template>inner</template></div>"),
            "nested template not preserved inside the region; got {:?}",
            html
        );
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::JavaScript(js) if js.contains("const after"))),
            "TS after the outer close was swallowed; got {:?}",
            kinds
        );
    }

    #[test]
    fn no_template_wrapper_interleaves_ts_and_html() {
        // FIX #2: with NO <template> wrapper, TS and HTML freely interleave: TS, an HTML element,
        // more TS, more HTML. The HTML elements become HTML regions; the TS stays JavaScript.
        let src = "const a = 1;\n<header>top</header>\nconst b = 2;\n<footer>bot</footer>";
        let kinds = lex(src);
        let htmls: Vec<&String> = kinds
            .iter()
            .filter_map(|k| match k {
                TokenKind::HTML(h) => Some(h),
                _ => None,
            })
            .collect();
        assert!(
            htmls.iter().any(|h| h.contains("<header>top</header>")),
            "first HTML region missing; got {:?}",
            kinds
        );
        assert!(
            htmls.iter().any(|h| h.contains("<footer>bot</footer>")),
            "second HTML region missing; got {:?}",
            kinds
        );
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::JavaScript(js) if js.contains("const a"))),
            "first TS region missing; got {:?}",
            kinds
        );
        assert!(
            kinds.iter().any(|k| matches!(k, TokenKind::JavaScript(js) if js.contains("const b"))),
            "interleaved TS region missing; got {:?}",
            kinds
        );
    }

    // ───────────────────────────── R2 balanced-scanner rough edges ─────────────────────────────
    //
    // Each test below pins a single rough edge the lexer audit called out. The shared helpers
    // reconstruct the joined JavaScript / HTML regions exactly the way `sfc::split_chunks` does
    // (concatenating same-kind token bodies), so a test asserts on the SAME text the compiler
    // downstream actually receives — never a regex over the source.

    /// Join all `JavaScript` token bodies in order (mirrors `sfc::split_chunks`' `javascript` bucket).
    fn joined_js(input: &str) -> String {
        lex(input)
            .into_iter()
            .filter_map(|k| match k {
                TokenKind::JavaScript(js) => Some(js),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Join all `HTML` token bodies in order (mirrors `sfc::split_chunks`' `html` bucket).
    fn joined_html(input: &str) -> String {
        lex(input)
            .into_iter()
            .filter_map(|k| match k {
                TokenKind::HTML(h) => Some(h),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn regex_literal_with_close_tag_stays_in_js() {
        // A regex whose body contains `</` (and `>`) must NOT split the JS region: the `</` is regex
        // content, not the start of an HTML close tag. The whole `/<\/div>/g` lands in JavaScript and
        // the following `<div>hi</div>` is its own intact HTML region.
        let src = "const re = /<\\/div>/g;\n<div>hi</div>";
        let js = joined_js(src);
        assert!(
            js.contains("/<\\/div>/g"),
            "regex literal was torn out of the JS region; joined JS = {js:?}"
        );
        let html = joined_html(src);
        assert_eq!(
            html.trim(),
            "<div>hi</div>",
            "HTML region was corrupted by the regex's `</`; got {html:?}"
        );
    }

    #[test]
    fn regex_literal_with_metachars_does_not_corrupt_region() {
        // A regex literal `/[<{;]/` carries every byte the old scanner treated as a hard boundary
        // (`<`, `{`, `;`). All of it must stay inside the single JavaScript region.
        let src = "const ok = /[<{;]/.test(input);\n<p>done</p>";
        let js = joined_js(src);
        assert!(
            js.contains("/[<{;]/.test(input)"),
            "regex metachars split the JS region; joined JS = {js:?}"
        );
        assert!(
            joined_html(src).contains("<p>done</p>"),
            "trailing HTML lost after a metachar regex"
        );
    }

    #[test]
    fn division_after_value_is_not_a_regex() {
        // Conservative regex disambiguation: a `/` after a value (ident / `)` / digit) is DIVISION,
        // not a regex start, so the following `/ b` is not swallowed as a regex body.
        let src = "const r = (a) / b / c;\n<div>x</div>";
        let js = joined_js(src);
        assert!(
            js.contains("(a) / b / c"),
            "division was misread as a regex; joined JS = {js:?}"
        );
        assert!(joined_html(src).contains("<div>x</div>"), "trailing HTML lost after division");
    }

    #[test]
    fn string_containing_close_template_is_safe() {
        // A string literal whose contents include `</template>` (or `</style>`, or `{{`) must not be
        // mistaken for a region boundary — the whole literal stays in the JS region.
        let src = "const s = \"</template> and {{ not interp }} and </style>\";\n<div>ok</div>";
        let js = joined_js(src);
        assert!(
            js.contains("\"</template> and {{ not interp }} and </style>\""),
            "a boundary-shaped string literal was split; joined JS = {js:?}"
        );
        assert!(joined_html(src).contains("<div>ok</div>"), "trailing HTML lost after the string");
    }

    #[test]
    fn multiline_balanced_expression_is_not_split_mid_braces() {
        // A multi-line arrow with a nested block (`computed(() => { … })`) spans newlines and `;`
        // INSIDE balanced brackets. The depth-aware scanner must keep it whole rather than breaking
        // at the first interior newline / `;`. The `gauge.treaty` `ratio` computed is exactly this.
        let src = "const ratio = computed(() => {\n  const span = max() - min();\n  return span <= 0 ? 0 : 1;\n});\n<div>v</div>";
        let js = joined_js(src);
        assert!(
            js.contains("const span = max() - min();") && js.contains("return span <= 0 ? 0 : 1;"),
            "balanced multi-line body was split mid-expression; joined JS = {js:?}"
        );
        assert!(joined_html(src).contains("<div>v</div>"), "trailing HTML lost after the arrow body");
    }

    #[test]
    fn nested_template_literal_substitution_is_balanced() {
        // A template literal with a `${ … }` substitution that itself contains a nested template
        // literal and an object brace must be consumed whole; its inner `}` / `` ` `` must not close
        // the outer literal early, and the following HTML must survive.
        let src = "const msg = `Hi ${ user({ id }) } and ${ `nested ${x}` }!`;\n<span>m</span>";
        let js = joined_js(src);
        assert!(
            js.contains("`Hi ${ user({ id }) } and ${ `nested ${x}` }!`"),
            "nested template literal was split; joined JS = {js:?}"
        );
        assert!(joined_html(src).contains("<span>m</span>"), "trailing HTML lost after template literal");
    }

    #[test]
    fn void_element_without_slash_does_not_swallow_trailing_ts() {
        // A bare `<br>` / `<input>` (no trailing `/`) is a VOID element: it must NOT keep the HTML
        // region open. The old scanner pushed it and waited for a `</br>` that never comes, swallowing
        // the rest of the file. Here the void element is nested in a container; the container closes
        // and the trailing TS stays JavaScript.
        let src = "<div>before<br>after<input name=\"x\">end</div>\nconst tail = 1;";
        let html = joined_html(src);
        assert!(
            html.contains("<br>") && html.contains("<input name=\"x\">") && html.contains("end"),
            "void elements + following text not captured in the HTML region; got {html:?}"
        );
        assert!(
            !html.contains("const tail"),
            "void element swallowed the trailing TS into the HTML region; got {html:?}"
        );
        assert!(
            joined_js(src).contains("const tail = 1"),
            "trailing TS after a void element was lost; joined JS = {}",
            joined_js(src)
        );
    }

    #[test]
    fn lone_void_root_element_does_not_run_to_eof() {
        // Even a void element as the ROOT must terminate the HTML region immediately (it has no end
        // tag), so a following TS region is preserved rather than swallowed to EOF.
        let src = "<hr>\nconst after = 2;";
        assert!(
            joined_js(src).contains("const after = 2"),
            "a root void element swallowed the rest of the file; joined JS = {}",
            joined_js(src)
        );
    }

    #[test]
    fn less_than_inside_interpolation_is_not_a_tag() {
        // `<` inside `{{ … }}` is interpolation text, not a tag open. The whole element (including the
        // interpolation and its closing tag) must be ONE HTML region.
        let src = "<div>{{ a < b }}</div>\nconst z = 9;";
        let html = joined_html(src);
        assert!(
            html.contains("<div>{{ a < b }}</div>"),
            "interpolation `<` was misread as a tag, splitting the region; got {html:?}"
        );
        assert!(
            joined_js(src).contains("const z = 9"),
            "trailing TS lost after interpolation with `<`; joined JS = {}",
            joined_js(src)
        );
    }

    #[test]
    fn greater_than_inside_interpolation_is_not_a_tag_end() {
        // The companion case: `>` inside an interpolation must not be mistaken for a tag end, and a
        // `}}` inside a string in the interpolation must not end it early.
        let src = "<p>{{ a > b ? \"}}\" : x }}</p>\nconst w = 1;";
        let html = joined_html(src);
        assert!(
            html.contains("<p>{{ a > b ? \"}}\" : x }}</p>"),
            "interpolation with `>` / a `}}`-bearing string was mis-segmented; got {html:?}"
        );
        assert!(joined_js(src).contains("const w = 1"), "trailing TS lost");
    }

    #[test]
    fn regex_disambiguation_helper() {
        // Direct table test of the regex-vs-divide decision used by `parse_javascript`.
        // Regex position (previous token cannot end an expression, or start of region):
        assert!(regex_allowed_after(None));
        assert!(regex_allowed_after(Some('=')));
        assert!(regex_allowed_after(Some('(')));
        assert!(regex_allowed_after(Some(',')));
        assert!(regex_allowed_after(Some('{')));
        assert!(regex_allowed_after(Some('!')));
        assert!(regex_allowed_after(Some('&')));
        // Divide position (previous token ends an expression):
        assert!(!regex_allowed_after(Some('a')));
        assert!(!regex_allowed_after(Some('Z')));
        assert!(!regex_allowed_after(Some('0')));
        assert!(!regex_allowed_after(Some('_')));
        assert!(!regex_allowed_after(Some('$')));
        assert!(!regex_allowed_after(Some(')')));
        assert!(!regex_allowed_after(Some(']')));
        assert!(!regex_allowed_after(Some('}')));
        assert!(!regex_allowed_after(Some('\'')));
        assert!(!regex_allowed_after(Some('"')));
        assert!(!regex_allowed_after(Some('`')));
    }

    #[test]
    fn void_element_set_is_case_insensitive() {
        assert!(is_void_element("br"));
        assert!(is_void_element("BR"));
        assert!(is_void_element("Input"));
        assert!(is_void_element("IMG"));
        assert!(!is_void_element("div"));
        assert!(!is_void_element("span"));
        assert!(!is_void_element("app-widget"));
    }
}