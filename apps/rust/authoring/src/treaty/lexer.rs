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
}

pub struct Lexer<'a> {
    input: &'a str,
    chars: Chars<'a>,
    pos: usize,
    current_char: Option<char>,
    state: LexerState,
    state_stack: Vec<LexerState>, // Stack to keep track of parent states
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

        match current_char {
            '<' if self.starts_with("<style>") => {
                self.advance_by("<style>".len());
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
                '<' if (self.starts_with("<style>") || self.starts_with("</")) => break,
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

    /// Parses a style block.
    fn parse_style(&mut self) -> Option<Token> {
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
        Some(Token::new(TokenKind::Style(value), start_pos, end_pos))
    }

    /// Parses an HTML segment.
    fn parse_html(&mut self) -> Option<Token> {
        let start_pos = self.pos;
        let mut tag_stack = Vec::new();

        while let Some(ch) = self.current_char {
            self.consume_whitespace();
            if ch == '<' {
                if self.starts_with("<!--") {
                    self.consume_html_comment();
                    continue;
                } else if self.starts_with("</") {
                    self.advance_by(2); // Skip '</'
                    let tag_name = self.consume_tag_name();
                    if let Some(expected_tag) = tag_stack.pop() {
                        if tag_name != expected_tag {
                            // Handle mismatched tag (optional)
                        }
                    } else {
                        break;
                    }
                    self.consume_until('>'); // Skip until '>'
                    self.advance(); // Skip '>'
                    if tag_stack.is_empty() {
                        break;
                    }
                } else if self.starts_with("<") {
                    self.advance(); // Skip '<'
                    let tag_name = self.consume_tag_name();
                    tag_stack.push(tag_name);
                    self.consume_attributes(); // Handle attributes (optional)
                } else {
                    self.advance();
                }
            } else if ch == '{' && self.starts_with("{{") {
                break;
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
        while let Some(ch) = self.current_char {
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
        while let Some(ch) = self.current_char {
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
                '\'' | '"' => self.consume_string(ch),
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