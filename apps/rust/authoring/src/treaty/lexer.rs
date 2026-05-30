use crate::treaty::token::{Token, TokenKind};
use std::str::Chars;

use super::token::{ControlFlowKind, DeferKind};

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
        let rest = &self.input[self.pos..];
        if let Some(after) = rest.strip_prefix("<style") {
            // The next char must be whitespace, `>`, or `/` — otherwise it's e.g. `<styled>`.
            matches!(after.chars().next(), Some(c) if c.is_whitespace() || c == '>' || c == '/')
                || after.is_empty()
        } else {
            false
        }
    }

    /// Parses a JavaScript block.
    fn parse_javascript(&mut self) -> Option<Token> {
        let start_pos = self.pos;
        while let Some(ch) = self.current_char {
            match ch {
                // Handle string literals
                '\'' | '"' | '`' => {
                    self.consume_string(ch);
                }

                // Handle comments
                '/' => {
                    if self.starts_with("//") {
                        self.consume_line_comment();
                    } else if self.starts_with("/*") {
                        self.consume_block_comment();
                    } else {
                        self.advance();
                    }
                }
                '\n' | '\r' | '\u{000C}' | ';' => {
                    self.advance();
                    break;
                }
                '<' if (self.starts_with_style_open() || self.starts_with("</")) => break,
                '{' if self.starts_with("{{") => break,
                '@' => {
                    // Handle the '@' character and transition to control flow state
                    self.push_state(LexerState::ControlFlow);
                    break;
                }
                // Handle other cases
                _ => self.advance(),
            }
        }

        let end_pos = self.pos;
        let value = self.input[start_pos..end_pos].to_string();
        self.pop_state(); // Return to the previous state
        Some(Token::new(TokenKind::JavaScript(value), start_pos, end_pos))
    }

    /// Parses a `<style …>…</style>` block.
    ///
    /// The cursor is positioned at the opening `<style`. The opening tag (and any attributes) is
    /// consumed first, capturing an optional `lang="…"` preprocessor language; the CSS body then
    /// runs up to the closing `</style>`.
    fn parse_style(&mut self) -> Option<Token> {
        // Consume the opening `<style …>` tag and capture the `lang` attribute, if present.
        debug_assert!(self.starts_with("<style"));
        self.advance_by("<style".len());
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
        let value = self.input[start_pos..end_pos].to_string();

        if self.starts_with("</style>") {
            self.advance_by("</style>".len());
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
                    self.advance_by("lang".len());
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
        self.advance_by("```".len()); // Skip the opening fence

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
        let content = self.input[content_start..self.pos].to_string();

        if self.starts_with("```") {
            self.advance_by("```".len());
        }

        self.pop_state(); // Return to the previous state
        Some(Token::new(
            TokenKind::Macro { content, info },
            token_start,
            self.pos,
        ))
    }

    /// Parses an HTML segment.
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
                    tag_stack.push(tag_name);
                    // A self-closing tag (`<img ... />`) closes immediately; mirror the TS lexer
                    // and pop it back off so it does not keep the HTML region open.
                    if self.consume_attributes() {
                        tag_stack.pop();
                        if tag_stack.is_empty() {
                            break;
                        }
                    }
                }
            } else {
                self.advance();
            }
        }

        let end_pos = self.pos;
        let value = self.input[start_pos..end_pos].to_string();
        self.pop_state(); // Return to the previous state
        Some(Token::new(TokenKind::HTML(value), start_pos, end_pos))
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
        let value = self.input[start_pos..end_pos].to_string();
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
            self.advance_by("@if".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::If), start_pos, self.pos));
        } else if self.starts_with("@else if") {
            self.advance_by("@else if".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::ElseIf), start_pos, self.pos));
        } else if self.starts_with("@else") {
            self.advance_by("@else".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Else), start_pos, self.pos));
        } else if self.starts_with("@for") {
            self.advance_by("@for".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::For), start_pos, self.pos));
        } else if self.starts_with("@empty") {
            self.advance_by("@empty".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Empty), start_pos, self.pos));
        } else if self.starts_with("@switch") {
            self.advance_by("@switch".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Switch), start_pos, self.pos));
        } else if self.starts_with("@case") {
            self.advance_by("@case".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Case), start_pos, self.pos));
        } else if self.starts_with("@default") {
            self.advance_by("@default".len());
            return Some(Token::new(TokenKind::ControlFlow(ControlFlowKind::Default), start_pos, self.pos));
        } else if self.starts_with("@defer") {
            self.advance_by("@defer".len());
            return Some(Token::new(TokenKind::Defer(DeferKind::Defer), start_pos, self.pos));
        } else if self.starts_with("@placeholder") {
            self.advance_by("@placeholder".len());
            return Some(Token::new(TokenKind::Defer(DeferKind::Placeholder), start_pos, self.pos));
        } else if self.starts_with("@loading") {
            self.advance_by("@loading".len());
            return Some(Token::new(TokenKind::Defer(DeferKind::Loading), start_pos, self.pos));
        } else if self.starts_with("@error") {
            self.advance_by("@error".len());
            return Some(Token::new(TokenKind::Defer(DeferKind::Error), start_pos, self.pos));
        } else {
            // If not a recognized control flow, assume it's JavaScript
            self.state = LexerState::JavaScript;
            self.advance(); // Ensure we advance the position to avoid infinite loop
            self.parse_javascript()
        }
    }

    /// Checks if the upcoming characters match the given string.
    fn starts_with(&self, s: &str) -> bool {
        self.input[self.pos..].starts_with(s)
    }

    /// Like [`Self::starts_with`] but ASCII-case-insensitive (used for attribute names).
    fn starts_with_ignore_ascii_case(&self, s: &str) -> bool {
        let rest = &self.input[self.pos..];
        rest.len() >= s.len() && rest.as_bytes()[..s.len()].eq_ignore_ascii_case(s.as_bytes())
    }

    /// Advances the lexer by a given number of bytes.
    fn advance_by(&mut self, n: usize) {
        for _ in 0..n {
            self.advance();
        }
    }

    /// Consumes a string literal, handling escaped characters.
    fn consume_string(&mut self, delimiter: char) {
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
        self.advance_by("<!--".len());
        while self.current_char.is_some() {
            if self.starts_with("-->") {
                self.advance_by("-->".len());
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
}