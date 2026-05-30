//! Lexer for Angular binding expressions.
//! PORT TARGET: `migration/render3-specs/03-expr_lexer.md`
//! Source: `tools/angular-ref/packages/compiler/src/expression_parser/lexer.ts`
//!         (and `tools/angular-ref/packages/compiler/src/chars.ts`)
//!
//! Self-contained scanner that tokenizes the JS-like micro-language used inside
//! Angular template bindings (`{{ }}`, `[x]="..."`, `(y)="..."`, `*ngFor`,
//! `@if`/`@for` block expressions). Produces a flat `Vec<Token>` consumed by the
//! expression parser.
//!
//! Fidelity notes vs. the TypeScript original:
//! - We operate over UTF-16 code units (`&[u16]`), matching `charCodeAt`/`.length`
//!   semantics so `index`/`end` offsets line up with Angular's source-map machinery.
//! - `index`/`end` are `i32` to preserve the `-1` sentinel used by `EOF`.
//! - The overloaded `numValue`/`strValue` pair becomes a tagged enum (`TokenValue`).
//! - Lexical errors are emitted inline as `TokenValue::Error`; the parser decides
//!   how to surface them. (`parse_int_auto_radix` is the one path that can panic,
//!   mirroring the TS `throw`; it should be unreachable given prior validation.)

// ---------------------------------------------------------------------------
// chars: code-point constants + predicates (port of `../chars`)
// ---------------------------------------------------------------------------

mod chars {
    pub const EOF: u32 = 0;
    pub const TAB: u32 = 9;
    pub const LF: u32 = 10;
    pub const VTAB: u32 = 11;
    pub const FF: u32 = 12;
    pub const CR: u32 = 13;
    pub const SPACE: u32 = 32;
    pub const BANG: u32 = 33;
    pub const DQ: u32 = 34;
    pub const HASH: u32 = 35;
    pub const DOLLAR: u32 = 36; // `$$` in chars.ts
    pub const PERCENT: u32 = 37;
    pub const AMPERSAND: u32 = 38;
    pub const SQ: u32 = 39;
    pub const LPAREN: u32 = 40;
    pub const RPAREN: u32 = 41;
    pub const STAR: u32 = 42;
    pub const PLUS: u32 = 43;
    pub const COMMA: u32 = 44;
    pub const MINUS: u32 = 45;
    pub const PERIOD: u32 = 46;
    pub const SLASH: u32 = 47;
    pub const COLON: u32 = 58;
    pub const SEMICOLON: u32 = 59;
    pub const LT: u32 = 60;
    pub const EQ: u32 = 61;
    pub const GT: u32 = 62;
    pub const QUESTION: u32 = 63;

    pub const D0: u32 = 48;
    pub const D9: u32 = 57;

    pub const UA: u32 = 65; // 'A'
    pub const UE: u32 = 69; // 'E'
    pub const UZ: u32 = 90; // 'Z'

    pub const LBRACKET: u32 = 91;
    pub const BACKSLASH: u32 = 92;
    pub const RBRACKET: u32 = 93;
    pub const CARET: u32 = 94;
    pub const UNDERSCORE: u32 = 95;

    pub const LA: u32 = 97; // 'a'
    pub const LE: u32 = 101; // 'e'
    pub const LF_LETTER: u32 = 102; // 'f'
    pub const LN: u32 = 110; // 'n'
    pub const LR: u32 = 114; // 'r'
    pub const LT_LETTER: u32 = 116; // 't'
    pub const LU: u32 = 117; // 'u'
    pub const LV: u32 = 118; // 'v'
    pub const LZ: u32 = 122; // 'z'

    pub const LBRACE: u32 = 123;
    pub const BAR: u32 = 124;
    pub const RBRACE: u32 = 125;
    pub const NBSP: u32 = 160;

    pub const BT: u32 = 96; // backtick

    #[inline]
    pub fn is_whitespace(code: u32) -> bool {
        (code >= TAB && code <= SPACE) || code == NBSP
    }

    #[inline]
    pub fn is_digit(code: u32) -> bool {
        D0 <= code && code <= D9
    }

    #[inline]
    pub fn is_ascii_letter(code: u32) -> bool {
        (code >= LA && code <= LZ) || (code >= UA && code <= UZ)
    }
}

// ---------------------------------------------------------------------------
// Token types
// ---------------------------------------------------------------------------

/// Categories of lexed token. A `Character` token carries the char code; `EOF`
/// is itself a `Character` token with code 0. `Error` carries the message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenType {
    Character,
    Identifier,
    PrivateIdentifier,
    Keyword,
    String,
    Operator,
    Number,
    RegExpBody,
    RegExpFlags,
    Error,
}

/// Sub-kind of a `String` token, distinguishing plain strings from template
/// literal segments.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StringTokenKind {
    Plain,
    /// Text segment ending just before a `${` interpolation hole.
    TemplateLiteralPart,
    /// Final text segment ending at the closing backtick.
    TemplateLiteralEnd,
}

/// The 14 reserved keywords recognized by the expression grammar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Keyword {
    Var,
    Let,
    As,
    Null,
    Undefined,
    True,
    False,
    If,
    Else,
    This,
    Typeof,
    Void,
    In,
    Instanceof,
}

impl Keyword {
    /// Classify an identifier slice as a keyword, mirroring `KEYWORDS.indexOf`.
    pub fn from_str(s: &str) -> Option<Keyword> {
        Some(match s {
            "var" => Keyword::Var,
            "let" => Keyword::Let,
            "as" => Keyword::As,
            "null" => Keyword::Null,
            "undefined" => Keyword::Undefined,
            "true" => Keyword::True,
            "false" => Keyword::False,
            "if" => Keyword::If,
            "else" => Keyword::Else,
            "this" => Keyword::This,
            "typeof" => Keyword::Typeof,
            "void" => Keyword::Void,
            "in" => Keyword::In,
            "instanceof" => Keyword::Instanceof,
            _ => return None,
        })
    }

    /// The source text of the keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            Keyword::Var => "var",
            Keyword::Let => "let",
            Keyword::As => "as",
            Keyword::Null => "null",
            Keyword::Undefined => "undefined",
            Keyword::True => "true",
            Keyword::False => "false",
            Keyword::If => "if",
            Keyword::Else => "else",
            Keyword::This => "this",
            Keyword::Typeof => "typeof",
            Keyword::Void => "void",
            Keyword::In => "in",
            Keyword::Instanceof => "instanceof",
        }
    }
}

/// Tagged payload of a [`Token`], fusing the TS `numValue`/`strValue`/`kind`
/// fields into a single safe enum.
#[derive(Clone, Debug, PartialEq)]
pub enum TokenValue {
    /// `numValue` = char code.
    Character(u32),
    Identifier(String),
    /// Includes the leading `#`.
    PrivateIdentifier(String),
    Keyword(Keyword),
    Str {
        value: String,
        kind: StringTokenKind,
    },
    Operator(String),
    /// `numValue` for a numeric literal (int or float).
    Number(f64),
    /// Regex body text, without the slashes (but the span includes them).
    RegExpBody(String),
    RegExpFlags(String),
    /// Full "Lexer Error: ..." message.
    Error(String),
}

/// A single lexed token. `index`/`end` are UTF-16 code-unit offsets into the
/// source (`i32` to preserve the `-1` sentinel used by [`EOF`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub index: i32,
    pub end: i32,
    pub kind: TokenValue,
}

impl Token {
    fn new(index: i32, end: i32, kind: TokenValue) -> Token {
        Token { index, end, kind }
    }

    /// The `TokenType` discriminant (mirrors TS `Token.type`).
    pub fn token_type(&self) -> TokenType {
        match self.kind {
            TokenValue::Character(_) => TokenType::Character,
            TokenValue::Identifier(_) => TokenType::Identifier,
            TokenValue::PrivateIdentifier(_) => TokenType::PrivateIdentifier,
            TokenValue::Keyword(_) => TokenType::Keyword,
            TokenValue::Str { .. } => TokenType::String,
            TokenValue::Operator(_) => TokenType::Operator,
            TokenValue::Number(_) => TokenType::Number,
            TokenValue::RegExpBody(_) => TokenType::RegExpBody,
            TokenValue::RegExpFlags(_) => TokenType::RegExpFlags,
            TokenValue::Error(_) => TokenType::Error,
        }
    }

    pub fn is_character(&self, code: u32) -> bool {
        matches!(self.kind, TokenValue::Character(c) if c == code)
    }

    pub fn is_number(&self) -> bool {
        matches!(self.kind, TokenValue::Number(_))
    }

    pub fn is_string(&self) -> bool {
        matches!(self.kind, TokenValue::Str { .. })
    }

    pub fn is_operator(&self, operator: &str) -> bool {
        matches!(self.kind, TokenValue::Operator(ref s) if s == operator)
    }

    pub fn is_identifier(&self) -> bool {
        matches!(self.kind, TokenValue::Identifier(_))
    }

    pub fn is_private_identifier(&self) -> bool {
        matches!(self.kind, TokenValue::PrivateIdentifier(_))
    }

    pub fn is_keyword(&self) -> bool {
        matches!(self.kind, TokenValue::Keyword(_))
    }

    fn is_keyword_eq(&self, kw: Keyword) -> bool {
        matches!(self.kind, TokenValue::Keyword(k) if k == kw)
    }

    pub fn is_keyword_let(&self) -> bool {
        self.is_keyword_eq(Keyword::Let)
    }

    pub fn is_keyword_as(&self) -> bool {
        self.is_keyword_eq(Keyword::As)
    }

    pub fn is_keyword_null(&self) -> bool {
        self.is_keyword_eq(Keyword::Null)
    }

    pub fn is_keyword_undefined(&self) -> bool {
        self.is_keyword_eq(Keyword::Undefined)
    }

    pub fn is_keyword_true(&self) -> bool {
        self.is_keyword_eq(Keyword::True)
    }

    pub fn is_keyword_false(&self) -> bool {
        self.is_keyword_eq(Keyword::False)
    }

    pub fn is_keyword_this(&self) -> bool {
        self.is_keyword_eq(Keyword::This)
    }

    pub fn is_keyword_typeof(&self) -> bool {
        self.is_keyword_eq(Keyword::Typeof)
    }

    pub fn is_keyword_void(&self) -> bool {
        self.is_keyword_eq(Keyword::Void)
    }

    pub fn is_keyword_in(&self) -> bool {
        self.is_keyword_eq(Keyword::In)
    }

    pub fn is_keyword_instance_of(&self) -> bool {
        self.is_keyword_eq(Keyword::Instanceof)
    }

    pub fn is_error(&self) -> bool {
        matches!(self.kind, TokenValue::Error(_))
    }

    pub fn is_reg_exp_body(&self) -> bool {
        matches!(self.kind, TokenValue::RegExpBody(_))
    }

    pub fn is_reg_exp_flags(&self) -> bool {
        matches!(self.kind, TokenValue::RegExpFlags(_))
    }

    /// Numeric value for `Number` tokens, otherwise `-1`.
    pub fn to_number(&self) -> f64 {
        match self.kind {
            TokenValue::Number(n) => n,
            _ => -1.0,
        }
    }

    pub fn is_template_literal_part(&self) -> bool {
        matches!(
            self.kind,
            TokenValue::Str { kind: StringTokenKind::TemplateLiteralPart, .. }
        )
    }

    pub fn is_template_literal_end(&self) -> bool {
        matches!(
            self.kind,
            TokenValue::Str { kind: StringTokenKind::TemplateLiteralEnd, .. }
        )
    }

    pub fn is_template_literal_interpolation_start(&self) -> bool {
        self.is_operator("${")
    }

    /// Mirrors TS `toString()`: textual form, or the number's decimal form.
    pub fn to_string_value(&self) -> Option<String> {
        match &self.kind {
            TokenValue::Character(c) => {
                Some(char::from_u32(*c).map(String::from).unwrap_or_default())
            }
            TokenValue::Identifier(s)
            | TokenValue::Operator(s)
            | TokenValue::PrivateIdentifier(s)
            | TokenValue::Str { value: s, .. }
            | TokenValue::Error(s)
            | TokenValue::RegExpBody(s)
            | TokenValue::RegExpFlags(s) => Some(s.clone()),
            TokenValue::Keyword(k) => Some(k.as_str().to_string()),
            TokenValue::Number(n) => Some(format_number(*n)),
        }
    }
}

/// The end-of-input sentinel token (`index == end == -1`). Equivalent to TS
/// `EOF = new Token(-1, -1, TokenType.Character, 0, '')`.
pub const EOF: Token = Token {
    index: -1,
    end: -1,
    kind: TokenValue::Character(0),
};

// ---------------------------------------------------------------------------
// Token factory helpers (mirror module-private `newXToken`)
// ---------------------------------------------------------------------------

fn new_character_token(index: i32, end: i32, code: u32) -> Token {
    Token::new(index, end, TokenValue::Character(code))
}

fn new_identifier_token(index: i32, end: i32, text: String) -> Token {
    Token::new(index, end, TokenValue::Identifier(text))
}

fn new_private_identifier_token(index: i32, end: i32, text: String) -> Token {
    Token::new(index, end, TokenValue::PrivateIdentifier(text))
}

fn new_keyword_token(index: i32, end: i32, kw: Keyword) -> Token {
    Token::new(index, end, TokenValue::Keyword(kw))
}

fn new_operator_token(index: i32, end: i32, text: &str) -> Token {
    Token::new(index, end, TokenValue::Operator(text.to_string()))
}

fn new_number_token(index: i32, end: i32, n: f64) -> Token {
    Token::new(index, end, TokenValue::Number(n))
}

fn new_error_token(index: i32, end: i32, message: String) -> Token {
    Token::new(index, end, TokenValue::Error(message))
}

fn new_reg_exp_body_token(index: i32, end: i32, text: String) -> Token {
    Token::new(index, end, TokenValue::RegExpBody(text))
}

fn new_reg_exp_flags_token(index: i32, end: i32, text: String) -> Token {
    Token::new(index, end, TokenValue::RegExpFlags(text))
}

fn new_string_token(index: i32, end: i32, value: String, kind: StringTokenKind) -> Token {
    Token::new(index, end, TokenValue::Str { value, kind })
}

// ---------------------------------------------------------------------------
// Lexer entry point
// ---------------------------------------------------------------------------

/// Public entry point. Tokenizes `text` and returns the owned token stream.
pub struct Lexer;

impl Lexer {
    pub fn new() -> Lexer {
        Lexer
    }

    pub fn tokenize(&self, text: &str) -> Vec<Token> {
        Scanner::new(text).scan()
    }
}

impl Default for Lexer {
    fn default() -> Self {
        Lexer::new()
    }
}

/// Convenience free function equivalent to `Lexer::new().tokenize(text)`.
pub fn tokenize(text: &str) -> Vec<Token> {
    Lexer::new().tokenize(text)
}

// ---------------------------------------------------------------------------
// Scanner
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum BraceKind {
    Interpolation,
    Expression,
}

struct Scanner {
    /// Source as UTF-16 code units (matches TS `charCodeAt`).
    input: Vec<u16>,
    length: usize,
    /// Current code unit, `chars::EOF` (0) when past the end.
    peek: u32,
    /// Index of `peek`; starts at -1, matches TS.
    index: i32,
    tokens: Vec<Token>,
    brace_stack: Vec<BraceKind>,
}

impl Scanner {
    fn new(text: &str) -> Scanner {
        let input: Vec<u16> = text.encode_utf16().collect();
        let length = input.len();
        let mut s = Scanner {
            input,
            length,
            peek: 0,
            index: -1,
            tokens: Vec::new(),
            brace_stack: Vec::new(),
        };
        s.advance();
        s
    }

    /// `input[i]` as a `u32` code unit, or `chars::EOF` when out of range.
    fn code_at(&self, i: i32) -> u32 {
        if i < 0 || (i as usize) >= self.length {
            chars::EOF
        } else {
            self.input[i as usize] as u32
        }
    }

    /// Decode `input[start..end]` (UTF-16 offsets) into an owned `String`.
    fn substring(&self, start: i32, end: i32) -> String {
        let start = start.max(0) as usize;
        let end = (end.max(0) as usize).min(self.length);
        if start >= end {
            return String::new();
        }
        String::from_utf16_lossy(&self.input[start..end])
    }

    fn scan(mut self) -> Vec<Token> {
        let mut token = self.scan_token();
        while let Some(t) = token {
            self.tokens.push(t);
            token = self.scan_token();
        }
        self.tokens
    }

    fn advance(&mut self) {
        self.index += 1;
        self.peek = if self.index as usize >= self.length {
            chars::EOF
        } else {
            self.input[self.index as usize] as u32
        };
    }

    fn scan_token(&mut self) -> Option<Token> {
        let length = self.length;
        let mut peek = self.peek;
        let mut index = self.index;

        // Skip whitespace (any control char <= SPACE).
        while peek <= chars::SPACE {
            index += 1;
            if index as usize >= length {
                peek = chars::EOF;
                break;
            } else {
                peek = self.input[index as usize] as u32;
            }
        }

        self.peek = peek;
        self.index = index;

        if index < 0 || index as usize >= length {
            return None;
        }

        if is_identifier_start(peek) {
            return Some(self.scan_identifier());
        }

        if chars::is_digit(peek) {
            return Some(self.scan_number(index));
        }

        let start = index;
        match peek {
            chars::PERIOD => {
                self.advance();
                if chars::is_digit(self.peek) {
                    return Some(self.scan_number(start));
                }
                if self.peek != chars::PERIOD {
                    return Some(new_character_token(start, self.index, chars::PERIOD));
                }
                self.advance();
                if self.peek == chars::PERIOD {
                    self.advance();
                    return Some(new_operator_token(start, self.index, "..."));
                }
                Some(self.error(&format!("Unexpected character [{}]", char_str(peek)), 0))
            }
            chars::LPAREN
            | chars::RPAREN
            | chars::LBRACKET
            | chars::RBRACKET
            | chars::COMMA
            | chars::COLON
            | chars::SEMICOLON => Some(self.scan_character(start, peek)),
            chars::LBRACE => Some(self.scan_open_brace(start, peek)),
            chars::RBRACE => Some(self.scan_close_brace(start, peek)),
            chars::SQ | chars::DQ => Some(self.scan_string()),
            chars::BT => {
                self.advance();
                Some(self.scan_template_literal_part(start))
            }
            chars::HASH => Some(self.scan_private_identifier()),
            chars::PLUS => Some(self.scan_complex_operator(start, "+", chars::EQ, "=", None, None)),
            chars::MINUS => {
                Some(self.scan_complex_operator(start, "-", chars::EQ, "=", None, None))
            }
            chars::SLASH => {
                if self.is_start_of_regex() {
                    Some(self.scan_regex(index))
                } else {
                    Some(self.scan_complex_operator(start, "/", chars::EQ, "=", None, None))
                }
            }
            chars::PERCENT => {
                Some(self.scan_complex_operator(start, "%", chars::EQ, "=", None, None))
            }
            chars::CARET => Some(self.scan_operator(start, "^")),
            chars::STAR => Some(self.scan_star(start)),
            chars::QUESTION => Some(self.scan_question(start)),
            chars::LT | chars::GT => Some(self.scan_complex_operator(
                start,
                &char_str(peek),
                chars::EQ,
                "=",
                None,
                None,
            )),
            chars::BANG => Some(self.scan_complex_operator(
                start,
                "!",
                chars::EQ,
                "=",
                Some(chars::EQ),
                Some("="),
            )),
            chars::EQ => Some(self.scan_equals(start)),
            chars::AMPERSAND => Some(self.scan_complex_operator(
                start,
                "&",
                chars::AMPERSAND,
                "&",
                Some(chars::EQ),
                Some("="),
            )),
            chars::BAR => Some(self.scan_complex_operator(
                start,
                "|",
                chars::BAR,
                "|",
                Some(chars::EQ),
                Some("="),
            )),
            chars::NBSP => {
                while chars::is_whitespace(self.peek) {
                    self.advance();
                }
                self.scan_token()
            }
            _ => {
                self.advance();
                Some(self.error(&format!("Unexpected character [{}]", char_str(peek)), 0))
            }
        }
    }

    fn scan_character(&mut self, start: i32, code: u32) -> Token {
        self.advance();
        new_character_token(start, self.index, code)
    }

    fn scan_operator(&mut self, start: i32, str: &str) -> Token {
        self.advance();
        new_operator_token(start, self.index, str)
    }

    fn scan_open_brace(&mut self, start: i32, code: u32) -> Token {
        self.brace_stack.push(BraceKind::Expression);
        self.advance();
        new_character_token(start, self.index, code)
    }

    fn scan_close_brace(&mut self, start: i32, code: u32) -> Token {
        self.advance();
        let current_brace = self.brace_stack.pop();
        if current_brace == Some(BraceKind::Interpolation) {
            let tok = new_character_token(start, self.index, chars::RBRACE);
            self.tokens.push(tok);
            return self.scan_template_literal_part(self.index);
        }
        new_character_token(start, self.index, code)
    }

    /// Tokenize a 2/3-char operator.
    fn scan_complex_operator(
        &mut self,
        start: i32,
        one: &str,
        two_code: u32,
        two: &str,
        three_code: Option<u32>,
        three: Option<&str>,
    ) -> Token {
        self.advance();
        let mut str = String::from(one);
        if self.peek == two_code {
            self.advance();
            str.push_str(two);
        }
        if let Some(tc) = three_code {
            if self.peek == tc {
                self.advance();
                str.push_str(three.unwrap_or(""));
            }
        }
        new_operator_token(start, self.index, &str)
    }

    fn scan_equals(&mut self, start: i32) -> Token {
        self.advance();
        let mut str = String::from("=");
        if self.peek == chars::EQ {
            self.advance();
            str.push('=');
        } else if self.peek == chars::GT {
            self.advance();
            str.push('>');
            return new_operator_token(start, self.index, &str);
        }
        if self.peek == chars::EQ {
            self.advance();
            str.push('=');
        }
        new_operator_token(start, self.index, &str)
    }

    fn scan_identifier(&mut self) -> Token {
        let start = self.index;
        self.advance();
        while is_identifier_part(self.peek) {
            self.advance();
        }
        let str = self.substring(start, self.index);
        match Keyword::from_str(&str) {
            Some(kw) => new_keyword_token(start, self.index, kw),
            None => new_identifier_token(start, self.index, str),
        }
    }

    /// Scans an ECMAScript private identifier (`#foo`); text includes the `#`.
    fn scan_private_identifier(&mut self) -> Token {
        let start = self.index;
        self.advance();
        if !is_identifier_start(self.peek) {
            return self.error("Invalid character [#]", -1);
        }
        while is_identifier_part(self.peek) {
            self.advance();
        }
        let identifier_name = self.substring(start, self.index);
        new_private_identifier_token(start, self.index, identifier_name)
    }

    fn scan_number(&mut self, start: i32) -> Token {
        let mut simple = self.index == start;
        let mut has_separators = false;
        self.advance(); // Skip initial digit.
        loop {
            if chars::is_digit(self.peek) {
                // Do nothing.
            } else if self.peek == chars::UNDERSCORE {
                // Separators must be surrounded by digits.
                if !chars::is_digit(self.code_at(self.index - 1))
                    || !chars::is_digit(self.code_at(self.index + 1))
                {
                    return self.error("Invalid numeric separator", 0);
                }
                has_separators = true;
            } else if self.peek == chars::PERIOD {
                simple = false;
            } else if is_exponent_start(self.peek) {
                self.advance();
                if is_exponent_sign(self.peek) {
                    self.advance();
                }
                if !chars::is_digit(self.peek) {
                    return self.error("Invalid exponent", -1);
                }
                simple = false;
            } else {
                break;
            }
            self.advance();
        }

        let mut str = self.substring(start, self.index);
        if has_separators {
            str = str.replace('_', "");
        }
        let value = if simple {
            parse_int_auto_radix(&str)
        } else {
            parse_float_js(&str)
        };
        new_number_token(start, self.index, value)
    }

    fn scan_string(&mut self) -> Token {
        let start = self.index;
        let quote = self.peek;
        self.advance(); // Skip initial quote.

        let mut buffer = String::new();
        let mut marker = self.index;

        while self.peek != quote {
            if self.peek == chars::BACKSLASH {
                match self.scan_string_backslash(buffer, marker) {
                    Ok(b) => {
                        buffer = b;
                        marker = self.index;
                    }
                    Err(tok) => return tok,
                }
            } else if self.peek == chars::EOF {
                return self.error("Unterminated quote", 0);
            } else {
                self.advance();
            }
        }

        let last = self.substring(marker, self.index);
        self.advance(); // Skip terminating quote.
        buffer.push_str(&last);
        new_string_token(start, self.index, buffer, StringTokenKind::Plain)
    }

    fn scan_question(&mut self, start: i32) -> Token {
        self.advance();
        let mut operator = String::from("?");
        // `a ?? b`, `a ??= b`.
        if self.peek == chars::QUESTION {
            operator.push('?');
            self.advance();
            if self.peek == chars::EQ {
                operator.push('=');
                self.advance();
            }
        } else if self.peek == chars::PERIOD {
            // `a?.b`
            operator.push('.');
            self.advance();
        }
        new_operator_token(start, self.index, &operator)
    }

    fn scan_template_literal_part(&mut self, start: i32) -> Token {
        let mut buffer = String::new();
        let mut marker = self.index;

        while self.peek != chars::BT {
            if self.peek == chars::BACKSLASH {
                match self.scan_string_backslash(buffer, marker) {
                    Ok(b) => {
                        buffer = b;
                        marker = self.index;
                    }
                    Err(tok) => return tok,
                }
            } else if self.peek == chars::DOLLAR {
                let dollar = self.index;
                self.advance();
                if self.peek == chars::LBRACE {
                    self.brace_stack.push(BraceKind::Interpolation);
                    let mut part = buffer;
                    part.push_str(&self.substring(marker, dollar));
                    let part_tok = new_string_token(
                        start,
                        dollar,
                        part,
                        StringTokenKind::TemplateLiteralPart,
                    );
                    self.tokens.push(part_tok);
                    self.advance();
                    let text = self.substring(dollar, self.index);
                    return new_operator_token(dollar, self.index, &text);
                }
                // `$` not followed by `{`: fall through, `$` becomes literal text.
                // (No extra advance; loop continues from after the `$`.)
            } else if self.peek == chars::EOF {
                return self.error("Unterminated template literal", 0);
            } else {
                self.advance();
            }
        }

        let last = self.substring(marker, self.index);
        self.advance();
        buffer.push_str(&last);
        new_string_token(start, self.index, buffer, StringTokenKind::TemplateLiteralEnd)
    }

    fn error(&self, message: &str, offset: i32) -> Token {
        let position = self.index + offset;
        let input_str = String::from_utf16_lossy(&self.input);
        new_error_token(
            position,
            self.index,
            format!(
                "Lexer Error: {message} at column {position} in expression [{input_str}]"
            ),
        )
    }

    /// Handle a backslash escape inside a string / template literal. On success
    /// returns the extended buffer; on error returns the error token.
    fn scan_string_backslash(
        &mut self,
        mut buffer: String,
        marker: i32,
    ) -> Result<String, Token> {
        buffer.push_str(&self.substring(marker, self.index));
        let unescaped_code: u32;
        self.advance();
        if self.peek == chars::LU {
            // 4-char hex code for a unicode character.
            let hex = self.substring(self.index + 1, self.index + 5);
            if !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()) {
                unescaped_code = u32::from_str_radix(&hex, 16).unwrap_or(0);
            } else {
                return Err(self.error(&format!("Invalid unicode escape [\\u{hex}]"), 0));
            }
            for _ in 0..5 {
                self.advance();
            }
        } else {
            unescaped_code = unescape(self.peek);
            self.advance();
        }
        if let Some(c) = char::from_u32(unescaped_code) {
            buffer.push(c);
        }
        Ok(buffer)
    }

    fn scan_star(&mut self, start: i32) -> Token {
        self.advance();
        // `*`, `**`, `**=` or `*=`.
        let mut operator = String::from("*");
        if self.peek == chars::STAR {
            operator.push('*');
            self.advance();
            if self.peek == chars::EQ {
                operator.push('=');
                self.advance();
            }
        } else if self.peek == chars::EQ {
            operator.push('=');
            self.advance();
        }
        new_operator_token(start, self.index, &operator)
    }

    fn is_start_of_regex(&self) -> bool {
        if self.tokens.is_empty() {
            return true;
        }
        let prev_token = &self.tokens[self.tokens.len() - 1];

        // A slash preceded by `!` may be negation (`!/re/`, regex) or a non-null
        // assertion followed by division (`x!/2`). Regexes only follow negations.
        if prev_token.is_operator("!") {
            let before_prev = if self.tokens.len() > 1 {
                Some(&self.tokens[self.tokens.len() - 2])
            } else {
                None
            };
            let is_negation = match before_prev {
                None => true,
                Some(t) => {
                    t.token_type() != TokenType::Identifier
                        && !t.is_character(chars::RPAREN)
                        && !t.is_character(chars::RBRACKET)
                }
            };
            return is_negation;
        }

        // Otherwise a regex iff preceded by an operator, or `(`, `[`, `,`, `:`.
        prev_token.token_type() == TokenType::Operator
            || prev_token.is_character(chars::LPAREN)
            || prev_token.is_character(chars::LBRACKET)
            || prev_token.is_character(chars::COMMA)
            || prev_token.is_character(chars::COLON)
    }

    fn scan_regex(&mut self, token_start: i32) -> Token {
        self.advance();
        let text_start = self.index;
        let mut in_escape = false;
        let mut in_character_class = false;

        loop {
            let peek = self.peek;
            if peek == chars::EOF {
                return self.error("Unterminated regular expression", 0);
            }
            if in_escape {
                in_escape = false;
            } else if peek == chars::BACKSLASH {
                in_escape = true;
            } else if peek == chars::LBRACKET {
                in_character_class = true;
            } else if peek == chars::RBRACKET {
                in_character_class = false;
            } else if peek == chars::SLASH && !in_character_class {
                break;
            }
            self.advance();
        }

        // Body value excludes slashes, but the span still includes them.
        let value = self.substring(text_start, self.index);
        self.advance();
        let body_token = new_reg_exp_body_token(token_start, self.index, value);
        let flags_token = self.scan_regex_flags(self.index);

        match flags_token {
            Some(flags) => {
                self.tokens.push(body_token);
                flags
            }
            None => body_token,
        }
    }

    fn scan_regex_flags(&mut self, start: i32) -> Option<Token> {
        if !chars::is_ascii_letter(self.peek) {
            return None;
        }
        while chars::is_ascii_letter(self.peek) {
            self.advance();
        }
        let text = self.substring(start, self.index);
        Some(new_reg_exp_flags_token(start, self.index, text))
    }
}

// ---------------------------------------------------------------------------
// Char-class helpers (module-private in TS)
// ---------------------------------------------------------------------------

fn is_identifier_start(code: u32) -> bool {
    (chars::LA <= code && code <= chars::LZ)
        || (chars::UA <= code && code <= chars::UZ)
        || code == chars::UNDERSCORE
        || code == chars::DOLLAR
}

fn is_identifier_part(code: u32) -> bool {
    chars::is_ascii_letter(code)
        || chars::is_digit(code)
        || code == chars::UNDERSCORE
        || code == chars::DOLLAR
}

fn is_exponent_start(code: u32) -> bool {
    code == chars::LE || code == chars::UE
}

fn is_exponent_sign(code: u32) -> bool {
    code == chars::MINUS || code == chars::PLUS
}

fn unescape(code: u32) -> u32 {
    match code {
        chars::LN => chars::LF,
        chars::LF_LETTER => chars::FF,
        chars::LR => chars::CR,
        chars::LT_LETTER => chars::TAB,
        chars::LV => chars::VTAB,
        _ => code,
    }
}

/// Render a single code unit as a string for error messages (`String.fromCharCode`).
fn char_str(code: u32) -> String {
    char::from_u32(code).map(String::from).unwrap_or_default()
}

/// Mirror JS `Number.prototype.toString` enough for the cases the lexer emits:
/// integers print without a trailing `.0`, others use the shortest float repr.
fn format_number(n: f64) -> String {
    if n.fract() == 0.0 && n.is_finite() && n.abs() < 1e21 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Mirror JS `parseInt(text)` with auto radix: `0x`/`0X` prefix => base 16 on
/// the remainder, otherwise base 10, reading leading digits and ignoring
/// trailing junk. Panics on `NaN` (mirrors the TS `throw`; unreachable given
/// the scanner validated the numeric shape first).
fn parse_int_auto_radix(text: &str) -> f64 {
    let t = text.trim_start();
    let (radix, rest) = if let Some(stripped) = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
    {
        (16u32, stripped)
    } else {
        (10u32, t)
    };

    let is_radix_digit = |c: char| c.to_digit(radix).is_some();
    let digits: String = rest.chars().take_while(|&c| is_radix_digit(c)).collect();

    if digits.is_empty() {
        panic!("Invalid integer literal when parsing {text}");
    }
    match i128::from_str_radix(&digits, radix) {
        Ok(v) => v as f64,
        Err(_) => {
            // Overflow of i128: fall back to a digit-by-digit f64 accumulation.
            let mut acc = 0.0f64;
            let r = radix as f64;
            for c in digits.chars() {
                acc = acc * r + c.to_digit(radix).unwrap() as f64;
            }
            acc
        }
    }
}

/// Mirror JS `parseFloat`: parse the leading float, ignoring trailing junk.
/// The scanner pre-validates the shape, so the slice is normally clean.
fn parse_float_js(text: &str) -> f64 {
    if let Ok(v) = text.parse::<f64>() {
        return v;
    }
    // Trim trailing non-float characters and retry (JS leniency).
    let bytes = text.as_bytes();
    let mut end = bytes.len();
    while end > 0 {
        if let Ok(v) = text[..end].parse::<f64>() {
            return v;
        }
        end -= 1;
    }
    f64::NAN
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn ttypes(text: &str) -> Vec<TokenType> {
        tokenize(text).iter().map(|t| t.token_type()).collect()
    }

    #[test]
    fn tokenizes_interpolation_member_access_body() {
        // The body of `{{ a.b }}` as seen by the expression lexer is `a.b`.
        let toks = tokenize("a.b");
        assert_eq!(toks.len(), 3);
        assert!(toks[0].is_identifier());
        assert_eq!(toks[0].to_string_value().as_deref(), Some("a"));
        assert!(toks[1].is_character(chars::PERIOD));
        assert!(toks[2].is_identifier());
        assert_eq!(toks[2].to_string_value().as_deref(), Some("b"));
        // spans
        assert_eq!((toks[0].index, toks[0].end), (0, 1));
        assert_eq!((toks[1].index, toks[1].end), (1, 2));
        assert_eq!((toks[2].index, toks[2].end), (2, 3));
    }

    #[test]
    fn tokenizes_property_binding_brackets_and_assign() {
        // `[x]=1`
        let toks = tokenize("[x]=1");
        assert!(toks[0].is_character(chars::LBRACKET));
        assert!(toks[1].is_identifier());
        assert!(toks[2].is_character(chars::RBRACKET));
        assert!(toks[3].is_operator("="));
        assert!(toks[4].is_number());
        assert_eq!(toks[4].to_number(), 1.0);
    }

    #[test]
    fn tokenizes_event_binding_parens() {
        // `(y)=z`
        let toks = tokenize("(y)=z");
        assert!(toks[0].is_character(chars::LPAREN));
        assert!(toks[1].is_identifier());
        assert!(toks[2].is_character(chars::RPAREN));
        assert!(toks[3].is_operator("="));
        assert!(toks[4].is_identifier());
    }

    #[test]
    fn tokenizes_integers_floats_and_separators() {
        assert_eq!(tokenize("42")[0].to_number(), 42.0);
        assert_eq!(tokenize("3.14")[0].to_number(), 3.14);
        assert_eq!(tokenize(".5")[0].to_number(), 0.5);
        assert_eq!(tokenize("1_000")[0].to_number(), 1000.0);
        assert_eq!(tokenize("1e3")[0].to_number(), 1000.0);
        // Note: `scanNumber` stops at `x` (not a decimal digit), so `0xff`
        // lexes as Number(0) followed by Identifier("xff") — matching Angular's
        // lexer, which never feeds a `0x...` slice to parse_int_auto_radix.
        let hexish = tokenize("0xff");
        assert_eq!(hexish[0].to_number(), 0.0);
        assert!(hexish[1].is_identifier());
        assert_eq!(hexish[1].to_string_value().as_deref(), Some("xff"));
    }

    #[test]
    fn parse_int_auto_radix_handles_hex_slice() {
        // Direct unit check of the radix helper (the `0x` path is reachable only
        // if a clean hex slice is passed, which the scanner does not currently do).
        assert_eq!(parse_int_auto_radix("0xff"), 255.0);
        assert_eq!(parse_int_auto_radix("42"), 42.0);
    }

    #[test]
    fn invalid_numeric_separator_is_error() {
        let toks = tokenize("1_");
        assert!(toks[0].is_error());
    }

    #[test]
    fn tokenizes_plain_string_with_escapes() {
        let toks = tokenize("'a\\nb'");
        assert_eq!(toks.len(), 1);
        assert!(toks[0].is_string());
        assert_eq!(toks[0].to_string_value().as_deref(), Some("a\nb"));
    }

    #[test]
    fn tokenizes_double_quoted_string() {
        let toks = tokenize("\"hi\"");
        assert!(toks[0].is_string());
        assert_eq!(toks[0].to_string_value().as_deref(), Some("hi"));
    }

    #[test]
    fn unterminated_quote_is_error() {
        let toks = tokenize("'abc");
        assert!(toks[0].is_error());
    }

    #[test]
    fn tokenizes_unicode_escape() {
        let toks = tokenize("'\\u0041'");
        assert_eq!(toks[0].to_string_value().as_deref(), Some("A"));
    }

    #[test]
    fn classifies_keywords_vs_identifiers() {
        let toks = tokenize("null undefined true false this typeof void in instanceof let as var if else");
        for t in &toks {
            assert!(t.is_keyword(), "expected keyword, got {:?}", t);
        }
        assert!(toks[0].is_keyword_null());
        assert!(toks[1].is_keyword_undefined());
        assert!(toks[2].is_keyword_true());
        assert!(toks[3].is_keyword_false());
        assert!(toks[4].is_keyword_this());
        assert!(toks[5].is_keyword_typeof());
        assert!(toks[6].is_keyword_void());
        assert!(toks[7].is_keyword_in());
        assert!(toks[8].is_keyword_instance_of());
        assert!(toks[9].is_keyword_let());

        // Non-keyword identifier.
        assert!(tokenize("foo")[0].is_identifier());
    }

    #[test]
    fn tokenizes_operators() {
        assert!(tokenize("+")[0].is_operator("+"));
        assert!(tokenize("+=")[0].is_operator("+="));
        assert!(tokenize("===")[0].is_operator("==="));
        assert!(tokenize("=>")[0].is_operator("=>"));
        assert!(tokenize("==")[0].is_operator("=="));
        assert!(tokenize("!==")[0].is_operator("!=="));
        assert!(tokenize("!=")[0].is_operator("!="));
        assert!(tokenize("**")[0].is_operator("**"));
        assert!(tokenize("**=")[0].is_operator("**="));
        assert!(tokenize("*=")[0].is_operator("*="));
        assert!(tokenize("??")[0].is_operator("??"));
        assert!(tokenize("??=")[0].is_operator("??="));
        assert!(tokenize("?.")[0].is_operator("?."));
        assert!(tokenize("?")[0].is_operator("?"));
        assert!(tokenize("&&")[0].is_operator("&&"));
        assert!(tokenize("&=")[0].is_operator("&="));
        assert!(tokenize("||")[0].is_operator("||"));
        assert!(tokenize("|")[0].is_operator("|"));
        assert!(tokenize("...")[0].is_operator("..."));
        assert!(tokenize("<=")[0].is_operator("<="));
        assert!(tokenize(">=")[0].is_operator(">="));
    }

    #[test]
    fn tokenizes_private_identifier() {
        let toks = tokenize("#foo");
        assert!(toks[0].is_private_identifier());
        assert_eq!(toks[0].to_string_value().as_deref(), Some("#foo"));
        // lone `#` is an error
        assert!(tokenize("#")[0].is_error());
    }

    #[test]
    fn tokenizes_template_literal_with_interpolation() {
        // `\`a${x}b\``
        let toks = tokenize("`a${x}b`");
        // Expect: Str(part "a"), Operator("${"), Identifier(x), Character(}),
        //         Str(end "b").
        assert!(toks[0].is_template_literal_part());
        assert_eq!(toks[0].to_string_value().as_deref(), Some("a"));
        assert!(toks[1].is_operator("${"));
        assert!(toks[1].is_template_literal_interpolation_start());
        assert!(toks[2].is_identifier());
        assert!(toks[3].is_character(chars::RBRACE));
        assert!(toks[4].is_template_literal_end());
        assert_eq!(toks[4].to_string_value().as_deref(), Some("b"));
    }

    #[test]
    fn brace_stack_distinguishes_object_close_from_interpolation() {
        // `{a}` -> object-ish: `{`, ident, `}` (plain close brace, no template).
        let toks = tokenize("{a}");
        assert!(toks[0].is_character(chars::LBRACE));
        assert!(toks[1].is_identifier());
        assert!(toks[2].is_character(chars::RBRACE));
        assert_eq!(toks.len(), 3);
    }

    #[test]
    fn regex_vs_division_heuristic() {
        // After an operator: regex.
        let re = tokenize("= /ab+/g");
        assert!(re[0].is_operator("="));
        assert!(re[1].is_reg_exp_body());
        assert_eq!(re[1].to_string_value().as_deref(), Some("ab+"));
        assert!(re[2].is_reg_exp_flags());
        assert_eq!(re[2].to_string_value().as_deref(), Some("g"));

        // After an identifier: division.
        let div = tokenize("a / b");
        assert!(div[0].is_identifier());
        assert!(div[1].is_operator("/"));
        assert!(div[2].is_identifier());

        // Negation `!/re/` is a regex.
        let neg = tokenize("!/re/");
        assert!(neg[0].is_operator("!"));
        assert!(neg[1].is_reg_exp_body());

        // Non-null assertion then divide: `x!/2`.
        let nn = tokenize("x!/2");
        assert!(nn[0].is_identifier());
        assert!(nn[1].is_operator("!"));
        assert!(nn[2].is_operator("/"));
        assert!(nn[3].is_number());
    }

    #[test]
    fn nbsp_is_skipped() {
        // U+00A0 NBSP between two identifiers.
        let toks = tokenize("a\u{00A0}b");
        assert_eq!(ttypes("a\u{00A0}b"), vec![TokenType::Identifier, TokenType::Identifier]);
        assert!(toks[0].is_identifier());
        assert!(toks[1].is_identifier());
    }

    #[test]
    fn eof_constant_has_negative_sentinels() {
        assert_eq!(EOF.index, -1);
        assert_eq!(EOF.end, -1);
        assert!(EOF.is_character(0));
    }

    #[test]
    fn unexpected_character_is_error() {
        let toks = tokenize("@");
        assert!(toks[0].is_error());
    }

    #[test]
    fn empty_input_yields_no_tokens() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("   ").is_empty());
    }
}
