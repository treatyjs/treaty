//! Recursive-descent parser for Angular binding expressions.
//! PORT TARGET: `migration/render3-specs/05-expr_parser.md`
//! Source: `tools/angular-ref/packages/compiler/src/expression_parser/parser.ts`
//!
//! This is the recursive-descent parser that turns the raw text of a binding
//! (`[prop]="..."`, `(event)="..."`, `{{ ... }}`, `*ngFor="..."`, host bindings,
//! ICU switch expressions, ...) plus a lexer-produced token stream into an
//! Angular expression AST ([`AstNode`] / [`ExprKind`]) wrapped in an
//! [`AstWithSource`]. It emits no `ɵɵ` runtime instructions; its output is the
//! in-memory AST plus a list of [`ParseError`]s.
//!
//! Per `migration/PORT-ARCHITECTURE.md`, the AST is OWNED and arena-free
//! (`Box`/`Vec`/`String`), so this port does not use a bump arena. Offsets are
//! `u32` in the AST spans (matching [`ParseSpan`]/[`AbsoluteSourceSpan`]), but
//! the parser does internal index arithmetic in `i32` (matching the lexer's
//! `Token::index`/`end` and the `-1` `EOF` sentinel), clamping to `0` when it
//! materializes a span.
//!
//! Fidelity notes:
//! - The `span()` start/end swap workaround (an acknowledged upstream parser
//!   bug) is reproduced verbatim so span output matches the TS reference.
//! - The `r*Expected` recovery counters and the `skip()` recovery points are
//!   replicated exactly; diverging would change error recovery.
//! - The `sourceSpanCache` keying (`start @ inputIndex : artificialEnd`) is
//!   reproduced with a tuple key, encoding the `undefined` artificial-end as a
//!   sentinel.

use super::ast::{
    ArrowFunctionIdentifierParameter, ArrowFunctionParameter, AstNode, AstWithSource,
    AbsoluteSourceSpan, BinaryOperation, BindingPipeType, ExprKind, LiteralMapKey, LiteralValue,
    ParseError, ParseSourceSpan, ParseSpan, TemplateBinding, TemplateBindingIdentifier,
    TemplateLiteralElement,
};
use super::lexer::{Lexer, StringTokenKind, Token, TokenType, TokenValue, EOF};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// chars: the handful of char-code constants the parser references directly.
// ---------------------------------------------------------------------------

mod chars {
    pub const SLASH: u32 = 47;
    pub const LPAREN: u32 = 40;
    pub const RPAREN: u32 = 41;
    pub const COMMA: u32 = 44;
    pub const COLON: u32 = 58;
    pub const SEMICOLON: u32 = 59;
    pub const PERIOD: u32 = 46;
    pub const LBRACKET: u32 = 91;
    pub const RBRACKET: u32 = 93;
    pub const LBRACE: u32 = 123;
    pub const RBRACE: u32 = 125;
    pub const UNDERSCORE: u32 = 95; // `$_`
    pub const UA: u32 = 65; // 'A'
    pub const UZ: u32 = 90; // 'Z'
    pub const SQ: u32 = 39;
    pub const DQ: u32 = 34;
    pub const BACKTICK: u32 = 96;

    /// Mirrors `chars.isQuote`.
    pub fn is_quote(code: u32) -> bool {
        code == SQ || code == DQ || code == BACKTICK
    }
}

// ---------------------------------------------------------------------------
// Public types (exported surface of parser.ts).
// ---------------------------------------------------------------------------

/// `InterpolationPiece` — a `{ text, start, end }` slice produced while
/// splitting an interpolation string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterpolationPiece {
    pub text: String,
    pub start: usize,
    pub end: usize,
}

/// `SplitInterpolation` — the result of [`Parser::split_interpolation`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitInterpolation {
    pub strings: Vec<InterpolationPiece>,
    pub expressions: Vec<InterpolationPiece>,
    pub offsets: Vec<usize>,
}

/// `TemplateBindingParseResult` — bindings + warnings + errors from
/// [`Parser::parse_template_bindings`].
#[derive(Clone, Debug, PartialEq)]
pub struct TemplateBindingParseResult {
    pub template_bindings: Vec<TemplateBinding>,
    pub warnings: Vec<String>,
    pub errors: Vec<ParseError>,
}

/// `ParseFlags` bitmask. `Action` marks an output/event binding (assignments &
/// chains allowed, pipes forbidden).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseFlags(u8);

impl ParseFlags {
    pub const NONE: ParseFlags = ParseFlags(0);
    pub const ACTION: ParseFlags = ParseFlags(1 << 0);

    #[inline]
    fn contains_action(self) -> bool {
        self.0 & ParseFlags::ACTION.0 != 0
    }
}

/// `ParseContextFlags` — a stateful context the parser is in. `Writable` means
/// a value may be written to an lvalue (an `=` may follow a property access).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ParseContextFlags(u8);

impl ParseContextFlags {
    const NONE: ParseContextFlags = ParseContextFlags(0);
    const WRITABLE: ParseContextFlags = ParseContextFlags(1);

    #[inline]
    fn is_writable(self) -> bool {
        self.0 & ParseContextFlags::WRITABLE.0 != 0
    }
}

/// Possible flags that can be used in a regex literal (`SUPPORTED_REGEX_FLAGS`).
const SUPPORTED_REGEX_FLAGS: [char; 8] = ['d', 'g', 'i', 'm', 's', 'u', 'v', 'y'];

// ---------------------------------------------------------------------------
// getLocation / getParseError.
// ---------------------------------------------------------------------------

/// Mirrors `getLocation(span)`: `span.start.toString() || '(unknown)'`.
fn get_location(span: &ParseSourceSpan) -> String {
    // In TS `span.start` is a `ParseLocation` whose `toString()` is a non-empty
    // file:line:col string; here the placeholder span only has numeric offsets.
    // `0` stringifies to `"0"`, which is truthy as a string, so we never fall
    // back to `(unknown)` for offset 0 — matching `String(0) || '(unknown)'`.
    span.start.to_string()
}

/// Mirrors `getParseError`.
fn get_parse_error(
    message: &str,
    input: &str,
    location_text: &str,
    parse_source_span: &ParseSourceSpan,
) -> ParseError {
    let mut location_text = location_text.to_string();
    if !location_text.is_empty() {
        location_text = format!(" {location_text} ");
    }
    let location = get_location(parse_source_span);
    let msg = format!("Parser Error: {message}{location_text}[{input}] in {location}");
    ParseError {
        span: parse_source_span.clone(),
        msg,
    }
}

// ---------------------------------------------------------------------------
// Parser (public façade).
// ---------------------------------------------------------------------------

/// `Parser` — the public façade. Stateless apart from the (implicit) lexer and
/// the `supports_direct_pipe_references` flag. Each parse call builds a fresh
/// `_ParseAST` ([`ParseAst`]).
pub struct Parser {
    lexer: Lexer,
    supports_direct_pipe_references: bool,
}

impl Default for Parser {
    fn default() -> Self {
        Parser::new(Lexer::new())
    }
}

impl Parser {
    pub fn new(lexer: Lexer) -> Parser {
        Parser {
            lexer,
            supports_direct_pipe_references: false,
        }
    }

    pub fn with_direct_pipe_references(lexer: Lexer, supports: bool) -> Parser {
        Parser {
            lexer,
            supports_direct_pipe_references: supports,
        }
    }

    /// `parseAction`.
    pub fn parse_action(
        &self,
        input: &str,
        parse_source_span: ParseSourceSpan,
        absolute_offset: i32,
    ) -> AstWithSource {
        let mut errors: Vec<ParseError> = Vec::new();
        self.check_no_interpolation(&mut errors, input, &parse_source_span);
        let (source_to_lex, _) = self.strip_comments(input);
        let tokens = self.lexer.tokenize(&source_to_lex);
        let ast = ParseAst::new(
            input,
            parse_source_span.clone(),
            absolute_offset,
            tokens,
            ParseFlags::ACTION,
            &mut errors,
            0,
            self.supports_direct_pipe_references,
        )
        .parse_chain();
        AstWithSource::new(
            ast,
            Some(input.to_string()),
            get_location(&parse_source_span),
            absolute_offset.max(0) as u32,
            errors,
        )
    }

    /// `parseBinding`.
    pub fn parse_binding(
        &self,
        input: &str,
        parse_source_span: ParseSourceSpan,
        absolute_offset: i32,
    ) -> AstWithSource {
        let mut errors: Vec<ParseError> = Vec::new();
        let ast = self.parse_binding_ast(input, &parse_source_span, absolute_offset, &mut errors);
        AstWithSource::new(
            ast,
            Some(input.to_string()),
            get_location(&parse_source_span),
            absolute_offset.max(0) as u32,
            errors,
        )
    }

    fn check_simple_expression(&self, ast: &AstNode) -> Vec<String> {
        let mut checker = SimpleExpressionChecker::default();
        // SimpleExpressionChecker visits the AST; pipes append "pipes".
        use super::ast::AstVisitor;
        checker.visit(ast);
        checker.errors
    }

    /// `parseSimpleBinding` — host bindings; runs `SimpleExpressionChecker`
    /// (errors on any pipe).
    pub fn parse_simple_binding(
        &self,
        input: &str,
        parse_source_span: ParseSourceSpan,
        absolute_offset: i32,
    ) -> AstWithSource {
        let mut errors: Vec<ParseError> = Vec::new();
        let ast = self.parse_binding_ast(input, &parse_source_span, absolute_offset, &mut errors);
        let simpl_errors = self.check_simple_expression(&ast);
        if !simpl_errors.is_empty() {
            errors.push(get_parse_error(
                &format!(
                    "Host binding expression cannot contain {}",
                    simpl_errors.join(" ")
                ),
                input,
                "",
                &parse_source_span,
            ));
        }
        AstWithSource::new(
            ast,
            Some(input.to_string()),
            get_location(&parse_source_span),
            absolute_offset.max(0) as u32,
            errors,
        )
    }

    fn parse_binding_ast(
        &self,
        input: &str,
        parse_source_span: &ParseSourceSpan,
        absolute_offset: i32,
        errors: &mut Vec<ParseError>,
    ) -> AstNode {
        self.check_no_interpolation(errors, input, parse_source_span);
        let (source_to_lex, _) = self.strip_comments(input);
        let tokens = self.lexer.tokenize(&source_to_lex);
        ParseAst::new(
            input,
            parse_source_span.clone(),
            absolute_offset,
            tokens,
            ParseFlags::NONE,
            errors,
            0,
            self.supports_direct_pipe_references,
        )
        .parse_chain()
    }

    /// `parseTemplateBindings` — microsyntax (e.g. `*ngFor="let item of items"`).
    pub fn parse_template_bindings(
        &self,
        template_key: &str,
        template_value: &str,
        parse_source_span: ParseSourceSpan,
        absolute_key_offset: i32,
        absolute_value_offset: i32,
    ) -> TemplateBindingParseResult {
        let tokens = self.lexer.tokenize(template_value);
        let mut errors: Vec<ParseError> = Vec::new();
        let key = TemplateBindingIdentifier {
            source: template_key.to_string(),
            span: AbsoluteSourceSpan::new(
                absolute_key_offset.max(0) as u32,
                (absolute_key_offset.max(0) as u32) + template_key.encode_utf16().count() as u32,
            ),
        };
        let mut parser = ParseAst::new(
            template_value,
            parse_source_span,
            absolute_value_offset,
            tokens,
            ParseFlags::NONE,
            &mut errors,
            0,
            self.supports_direct_pipe_references,
        );
        parser.parse_template_bindings(key)
    }

    /// `parseInterpolation`. Returns `None` if there are no interpolations.
    pub fn parse_interpolation(
        &self,
        input: &str,
        parse_source_span: ParseSourceSpan,
        absolute_offset: i32,
    ) -> Option<AstWithSource> {
        let mut errors: Vec<ParseError> = Vec::new();
        let split = self.split_interpolation(input, &parse_source_span, &mut errors);
        if split.expressions.is_empty() {
            return None;
        }

        let mut expression_nodes: Vec<AstNode> = Vec::new();

        for i in 0..split.expressions.len() {
            let expression_text = &split.expressions[i].text;
            let (source_to_lex, has_comments) = self.strip_comments(expression_text);
            let tokens = self.lexer.tokenize(&source_to_lex);

            if has_comments && source_to_lex.trim().is_empty() && tokens.is_empty() {
                errors.push(get_parse_error(
                    "Interpolation expression cannot only contain a comment",
                    input,
                    &format!("at column {} in", split.expressions[i].start),
                    &parse_source_span,
                ));
                continue;
            }

            let ast = ParseAst::new(
                input,
                parse_source_span.clone(),
                absolute_offset,
                tokens,
                ParseFlags::NONE,
                &mut errors,
                split.offsets[i] as i32,
                self.supports_direct_pipe_references,
            )
            .parse_chain();
            expression_nodes.push(ast);
        }

        Some(self.create_interpolation_ast(
            split.strings.iter().map(|s| s.text.clone()).collect(),
            expression_nodes,
            input,
            get_location(&parse_source_span),
            absolute_offset,
            errors,
        ))
    }

    /// `parseInterpolationExpression` — for ICU switch expressions; treats the
    /// provided string as a single expression with empty pre/suffix strings.
    pub fn parse_interpolation_expression(
        &self,
        expression: &str,
        parse_source_span: ParseSourceSpan,
        absolute_offset: i32,
    ) -> AstWithSource {
        let (source_to_lex, _) = self.strip_comments(expression);
        let tokens = self.lexer.tokenize(&source_to_lex);
        let mut errors: Vec<ParseError> = Vec::new();
        let ast = ParseAst::new(
            expression,
            parse_source_span.clone(),
            absolute_offset,
            tokens,
            ParseFlags::NONE,
            &mut errors,
            0,
            self.supports_direct_pipe_references,
        )
        .parse_chain();
        let strings = vec![String::new(), String::new()];
        self.create_interpolation_ast(
            strings,
            vec![ast],
            expression,
            get_location(&parse_source_span),
            absolute_offset,
            errors,
        )
    }

    fn create_interpolation_ast(
        &self,
        strings: Vec<String>,
        expressions: Vec<AstNode>,
        input: &str,
        location: String,
        absolute_offset: i32,
        errors: Vec<ParseError>,
    ) -> AstWithSource {
        let len = input.encode_utf16().count() as u32;
        let span = ParseSpan::new(0, len);
        let interpolation = AstNode::new(
            span,
            span.to_absolute(absolute_offset.max(0) as u32),
            ExprKind::Interpolation {
                strings,
                expressions,
            },
        );
        AstWithSource::new(
            interpolation,
            Some(input.to_string()),
            location,
            absolute_offset.max(0) as u32,
            errors,
        )
    }

    /// `splitInterpolation` — split text into raw strings and interpolated
    /// expressions. Operates on UTF-16 code-unit indices to match the lexer.
    pub fn split_interpolation(
        &self,
        input: &str,
        parse_source_span: &ParseSourceSpan,
        errors: &mut Vec<ParseError>,
    ) -> SplitInterpolation {
        let units: Vec<u16> = input.encode_utf16().collect();
        let mut strings: Vec<InterpolationPiece> = Vec::new();
        let mut expressions: Vec<InterpolationPiece> = Vec::new();
        let mut offsets: Vec<usize> = Vec::new();
        let mut i: usize = 0;
        let mut at_interpolation = false;
        let mut extend_last_string = false;
        let interp_start: Vec<u16> = "{{".encode_utf16().collect();
        let interp_end = "}}";

        while i < units.len() {
            if !at_interpolation {
                let start = i;
                match index_of(&units, &interp_start, i) {
                    Some(idx) => i = idx,
                    None => i = units.len(),
                }
                let text = utf16_substring(&units, start, i);
                strings.push(InterpolationPiece {
                    text,
                    start,
                    end: i,
                });
                at_interpolation = true;
            } else {
                let full_start = i;
                let expr_start = full_start + interp_start.len();
                let expr_end = self.get_interpolation_end_index(input, interp_end, expr_start);
                if expr_end == -1 {
                    at_interpolation = false;
                    extend_last_string = true;
                    break;
                }
                let expr_end = expr_end as usize;
                let full_end = expr_end + interp_end.encode_utf16().count();

                let text = utf16_substring(&units, expr_start, expr_end);
                if text.trim().is_empty() {
                    errors.push(get_parse_error(
                        "Blank expressions are not allowed in interpolated strings",
                        input,
                        &format!("at column {i} in"),
                        parse_source_span,
                    ));
                }
                expressions.push(InterpolationPiece {
                    text,
                    start: full_start,
                    end: full_end,
                });
                // No ENCODED_ENTITY remap is performed (no interpolatedTokens are
                // threaded in this port); the offset is just the expr-start.
                let offset = full_start + interp_start.len();
                offsets.push(offset);

                i = full_end;
                at_interpolation = false;
            }
        }

        if !at_interpolation {
            if extend_last_string {
                if let Some(piece) = strings.last_mut() {
                    piece.text.push_str(&utf16_substring(&units, i, units.len()));
                    piece.end = units.len();
                }
            } else {
                strings.push(InterpolationPiece {
                    text: utf16_substring(&units, i, units.len()),
                    start: i,
                    end: units.len(),
                });
            }
        }

        SplitInterpolation {
            strings,
            expressions,
            offsets,
        }
    }

    /// `wrapLiteralPrimitive`.
    pub fn wrap_literal_primitive(
        &self,
        input: Option<&str>,
        location: String,
        absolute_offset: i32,
    ) -> AstWithSource {
        let len = input.map_or(0, |s| s.encode_utf16().count() as u32);
        let span = ParseSpan::new(0, len);
        let value = match input {
            None => LiteralValue::Null,
            Some(s) => LiteralValue::Str(s.to_string()),
        };
        let lit = AstNode::new(
            span,
            span.to_absolute(absolute_offset.max(0) as u32),
            ExprKind::LiteralPrimitive { value },
        );
        AstWithSource::new(
            lit,
            input.map(|s| s.to_string()),
            location,
            absolute_offset.max(0) as u32,
            Vec::new(),
        )
    }

    /// `_stripComments` — returns `(stripped, has_comments)`.
    fn strip_comments(&self, input: &str) -> (String, bool) {
        match self.comment_start(input) {
            Some(i) => {
                let units: Vec<u16> = input.encode_utf16().collect();
                (utf16_substring(&units, 0, i), true)
            }
            None => (input.to_string(), false),
        }
    }

    /// `_commentStart` — index (UTF-16 units) of the first unquoted `//`, or None.
    fn comment_start(&self, input: &str) -> Option<usize> {
        let units: Vec<u16> = input.encode_utf16().collect();
        let mut outer_quote: Option<u32> = None;
        if units.len() == 0 {
            return None;
        }
        for i in 0..units.len().saturating_sub(1) {
            let ch = units[i] as u32;
            let next_ch = units[i + 1] as u32;
            if ch == chars::SLASH && next_ch == chars::SLASH && outer_quote.is_none() {
                return Some(i);
            }
            if outer_quote == Some(ch) {
                outer_quote = None;
            } else if outer_quote.is_none() && chars::is_quote(ch) {
                outer_quote = Some(ch);
            }
        }
        None
    }

    /// `_checkNoInterpolation` — push an error if a `{{ ... }}` pair appears.
    fn check_no_interpolation(
        &self,
        errors: &mut Vec<ParseError>,
        input: &str,
        parse_source_span: &ParseSourceSpan,
    ) {
        let mut start_index: i64 = -1;
        let mut end_index: i64 = -1;
        let starts_with_interp = input.starts_with("{{");

        for char_index in ForEachUnquotedChar::new(input, 0) {
            if start_index == -1 {
                if starts_with_interp {
                    start_index = char_index as i64;
                }
            } else {
                end_index = self.get_interpolation_end_index(input, "}}", char_index) as i64;
                if end_index > -1 {
                    break;
                }
            }
        }

        if start_index > -1 && end_index > -1 {
            errors.push(get_parse_error(
                "Got interpolation ({{}}) where expression was expected",
                input,
                &format!("at column {start_index} in"),
                parse_source_span,
            ));
        }
    }

    /// `_getInterpolationEndIndex` — UTF-16 index of the end of an interpolation
    /// expression while ignoring comments and quoted content; `-1` if none.
    fn get_interpolation_end_index(&self, input: &str, expression_end: &str, start: usize) -> i32 {
        let units: Vec<u16> = input.encode_utf16().collect();
        let end_units: Vec<u16> = expression_end.encode_utf16().collect();
        let comment: Vec<u16> = "//".encode_utf16().collect();
        for char_index in ForEachUnquotedChar::new(input, start) {
            if starts_with_at(&units, &end_units, char_index) {
                return char_index as i32;
            }
            if starts_with_at(&units, &comment, char_index) {
                // Nothing else matters after a comment; look directly for the end.
                return match index_of(&units, &end_units, char_index) {
                    Some(idx) => idx as i32,
                    None => -1,
                };
            }
        }
        -1
    }
}

/// `_forEachUnquotedChar` generator → an iterator over UTF-16 indices outside
/// of quotes, starting at `start`.
struct ForEachUnquotedChar {
    units: Vec<u16>,
    i: usize,
    current_quote: Option<u16>,
    escape_count: u32,
}

impl ForEachUnquotedChar {
    fn new(input: &str, start: usize) -> ForEachUnquotedChar {
        ForEachUnquotedChar {
            units: input.encode_utf16().collect(),
            i: start,
            current_quote: None,
            escape_count: 0,
        }
    }
}

impl Iterator for ForEachUnquotedChar {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while self.i < self.units.len() {
            let i = self.i;
            let ch = self.units[i];
            let ch_code = ch as u32;
            let mut yielded: Option<usize> = None;
            if chars::is_quote(ch_code)
                && (self.current_quote.is_none() || self.current_quote == Some(ch))
                && self.escape_count % 2 == 0
            {
                self.current_quote = if self.current_quote.is_none() {
                    Some(ch)
                } else {
                    None
                };
            } else if self.current_quote.is_none() {
                yielded = Some(i);
            }
            // backslash is 92
            self.escape_count = if ch_code == 92 {
                self.escape_count + 1
            } else {
                0
            };
            self.i += 1;
            if let Some(y) = yielded {
                return Some(y);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// _ParseAST — the recursive-descent state machine.
// ---------------------------------------------------------------------------

struct ParseAst<'e> {
    input: String,
    parse_source_span: ParseSourceSpan,
    absolute_offset: i32,
    tokens: Vec<Token>,
    parse_flags: ParseFlags,
    errors: &'e mut Vec<ParseError>,
    offset: i32,
    supports_direct_pipe_references: bool,

    rparens_expected: i32,
    rbrackets_expected: i32,
    rbraces_expected: i32,
    context: ParseContextFlags,
    source_span_cache: HashMap<(i32, i32, i32), AbsoluteSourceSpan>,
    index: usize,
    /// Length of `input` in UTF-16 code units (matches TS `input.length`).
    input_len: i32,
}

impl<'e> ParseAst<'e> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        input: &str,
        parse_source_span: ParseSourceSpan,
        absolute_offset: i32,
        tokens: Vec<Token>,
        parse_flags: ParseFlags,
        errors: &'e mut Vec<ParseError>,
        offset: i32,
        supports_direct_pipe_references: bool,
    ) -> ParseAst<'e> {
        let input_len = input.encode_utf16().count() as i32;
        ParseAst {
            input: input.to_string(),
            parse_source_span,
            absolute_offset,
            tokens,
            parse_flags,
            errors,
            offset,
            supports_direct_pipe_references,
            rparens_expected: 0,
            rbrackets_expected: 0,
            rbraces_expected: 0,
            context: ParseContextFlags::NONE,
            source_span_cache: HashMap::new(),
            index: 0,
            input_len,
        }
    }

    fn peek(&self, offset: i32) -> &Token {
        let i = self.index as i32 + offset;
        if i >= 0 && (i as usize) < self.tokens.len() {
            &self.tokens[i as usize]
        } else {
            &EOF
        }
    }

    fn next(&self) -> &Token {
        self.peek(0)
    }

    fn at_eof(&self) -> bool {
        self.index >= self.tokens.len()
    }

    /// Index of the next token (or the end of the last token at EOF), `+offset`.
    fn input_index(&self) -> i32 {
        if self.at_eof() {
            self.current_end_index()
        } else {
            self.next().index + self.offset
        }
    }

    /// End index of the last processed token (or the start of the first if none).
    fn current_end_index(&self) -> i32 {
        if self.index > 0 {
            let cur_token = self.peek(-1);
            return cur_token.end + self.offset;
        }
        if self.tokens.is_empty() {
            return self.input_len + self.offset;
        }
        self.next().index + self.offset
    }

    /// Absolute offset of the start of the current token.
    fn current_absolute_offset(&self) -> i32 {
        self.absolute_offset + self.input_index()
    }

    /// `span(start, artificialEndIndex?)`. Reproduces the start/end swap
    /// workaround verbatim.
    fn span(&self, start: i32, artificial_end_index: Option<i32>) -> ParseSpan {
        let mut start = start;
        let mut end_index = self.current_end_index();
        if let Some(a) = artificial_end_index {
            if a > self.current_end_index() {
                end_index = a;
            }
        }
        // Workaround for a deep-seated parser bug: swap if start > endIndex.
        if start > end_index {
            std::mem::swap(&mut start, &mut end_index);
        }
        ParseSpan::new(start.max(0) as u32, end_index.max(0) as u32)
    }

    fn source_span(&mut self, start: i32, artificial_end_index: Option<i32>) -> AbsoluteSourceSpan {
        let key = (
            start,
            self.input_index(),
            artificial_end_index.unwrap_or(-1),
        );
        if let Some(v) = self.source_span_cache.get(&key) {
            return *v;
        }
        let abs = self
            .span(start, artificial_end_index)
            .to_absolute(self.absolute_offset.max(0) as u32);
        self.source_span_cache.insert(key, abs);
        abs
    }

    fn advance(&mut self) {
        self.index += 1;
    }

    fn consume_optional_character(&mut self, code: u32) -> bool {
        if self.next().is_character(code) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn peek_keyword_let(&self) -> bool {
        self.next().is_keyword_let()
    }

    fn peek_keyword_as(&self) -> bool {
        self.next().is_keyword_as()
    }

    fn expect_character(&mut self, code: u32) {
        if self.consume_optional_character(code) {
            return;
        }
        let ch = char::from_u32(code).map(String::from).unwrap_or_default();
        self.error(&format!("Missing expected {ch}"), None);
    }

    fn consume_optional_operator(&mut self, op: &str) -> bool {
        if self.next().is_operator(op) {
            self.advance();
            true
        } else {
            false
        }
    }

    /// `isAssignmentOperator(token)` — true iff it's an Operator whose strValue
    /// is an assignment operation.
    fn is_assignment_operator(token: &Token) -> bool {
        if let TokenValue::Operator(s) = &token.kind {
            if let Some(op) = BinaryOperation::from_str(s) {
                return op.is_assignment();
            }
        }
        false
    }

    fn expect_operator(&mut self, operator: &str) {
        if self.consume_optional_operator(operator) {
            return;
        }
        self.error(&format!("Missing expected operator {operator}"), None);
    }

    fn pretty_print_token(tok: &Token) -> String {
        if std::ptr::eq(tok, &EOF) || (tok.index == -1 && tok.end == -1) {
            "end of input".to_string()
        } else {
            format!("token {}", tok.to_string_value().unwrap_or_default())
        }
    }

    fn expect_identifier_or_keyword(&mut self) -> Option<String> {
        let is_id = self.next().is_identifier();
        let is_kw = self.next().is_keyword();
        if !is_id && !is_kw {
            if self.next().is_private_identifier() {
                let tok = self.next().clone();
                self.report_error_for_private_identifier(&tok, Some("expected identifier or keyword"));
            } else {
                let msg = format!(
                    "Unexpected {}, expected identifier or keyword",
                    Self::pretty_print_token(self.next())
                );
                self.error(&msg, None);
            }
            return None;
        }
        let s = self.next().to_string_value();
        self.advance();
        s
    }

    fn expect_identifier_or_keyword_or_string(&mut self) -> String {
        let n_is_id = self.next().is_identifier();
        let n_is_kw = self.next().is_keyword();
        let n_is_str = self.next().is_string();
        if !n_is_id && !n_is_kw && !n_is_str {
            if self.next().is_private_identifier() {
                let tok = self.next().clone();
                self.report_error_for_private_identifier(
                    &tok,
                    Some("expected identifier, keyword or string"),
                );
            } else {
                let msg = format!(
                    "Unexpected {}, expected identifier, keyword, or string",
                    Self::pretty_print_token(self.next())
                );
                self.error(&msg, None);
            }
            return String::new();
        }
        let s = self.next().to_string_value().unwrap_or_default();
        self.advance();
        s
    }

    fn parse_chain(&mut self) -> AstNode {
        let mut exprs: Vec<AstNode> = Vec::new();
        let start = self.input_index();
        while self.index < self.tokens.len() {
            let expr = self.parse_pipe();
            exprs.push(expr);

            if self.consume_optional_character(chars::SEMICOLON) {
                if !self.parse_flags.contains_action() {
                    self.error("Binding expression cannot contain chained expression", None);
                }
                while self.consume_optional_character(chars::SEMICOLON) {}
            } else if self.index < self.tokens.len() {
                let error_index = self.index;
                let tok = self.next().to_string_value().unwrap_or_default();
                self.error(&format!("Unexpected token '{tok}'"), None);
                if self.index == error_index {
                    break;
                }
            }
        }
        if exprs.is_empty() {
            let artificial_start = self.offset;
            let artificial_end = self.offset + self.input_len;
            return AstNode::new(
                self.span(artificial_start, Some(artificial_end)),
                self.source_span(artificial_start, Some(artificial_end)),
                ExprKind::EmptyExpr,
            );
        }
        if exprs.len() == 1 {
            return exprs.into_iter().next().unwrap();
        }
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(span, source_span, ExprKind::Chain { expressions: exprs })
    }

    fn parse_pipe(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_expression();
        if self.consume_optional_operator("|") {
            if self.parse_flags.contains_action() {
                self.error("Cannot have a pipe in an action expression", None);
            }

            loop {
                let name_start = self.input_index();
                let name_opt = self.expect_identifier_or_keyword();
                let name_id: String;
                let name_span: AbsoluteSourceSpan;
                let mut full_span_end: Option<i32> = None;
                if let Some(n) = name_opt {
                    name_id = n;
                    name_span = self.source_span(name_start, None);
                } else {
                    name_id = String::new();
                    let fse = if self.next().index != -1 {
                        self.next().index
                    } else {
                        self.input_len + self.offset
                    };
                    full_span_end = Some(fse);
                    name_span = ParseSpan::new(fse.max(0) as u32, fse.max(0) as u32)
                        .to_absolute(self.absolute_offset.max(0) as u32);
                }

                let mut args: Vec<AstNode> = Vec::new();
                while self.consume_optional_character(chars::COLON) {
                    args.push(self.parse_expression());
                }
                let pipe_type = if self.supports_direct_pipe_references {
                    let char_code = name_id.encode_utf16().next().unwrap_or(0) as u32;
                    if char_code == chars::UNDERSCORE
                        || (char_code >= chars::UA && char_code <= chars::UZ)
                    {
                        BindingPipeType::ReferencedDirectly
                    } else {
                        BindingPipeType::ReferencedByName
                    }
                } else {
                    BindingPipeType::ReferencedByName
                };

                let span = self.span(start, None);
                let source_span = self.source_span(start, full_span_end);
                result = AstNode::new(
                    span,
                    source_span,
                    ExprKind::BindingPipe {
                        name_span,
                        exp: Box::new(result),
                        name: name_id,
                        args,
                        pipe_type,
                    },
                );

                if !self.consume_optional_operator("|") {
                    break;
                }
            }
        }
        result
    }

    fn parse_expression(&mut self) -> AstNode {
        self.parse_conditional()
    }

    fn parse_conditional(&mut self) -> AstNode {
        let start = self.input_index();
        let result = self.parse_logical_or();

        if self.consume_optional_operator("?") {
            let yes = self.parse_pipe();
            let no: AstNode;
            if !self.consume_optional_character(chars::COLON) {
                let end = self.input_index();
                let units: Vec<u16> = self.input.encode_utf16().collect();
                let expression = utf16_substring(&units, start.max(0) as usize, end.max(0) as usize);
                self.error(
                    &format!("Conditional expression {expression} requires all 3 expressions"),
                    None,
                );
                let span = self.span(start, None);
                let source_span = self.source_span(start, None);
                no = AstNode::new(span, source_span, ExprKind::EmptyExpr);
            } else {
                no = self.parse_pipe();
            }
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(
                span,
                source_span,
                ExprKind::Conditional {
                    condition: Box::new(result),
                    true_exp: Box::new(yes),
                    false_exp: Box::new(no),
                },
            );
        }
        result
    }

    /// Shared helper for the left-associative binary levels.
    fn make_binary(&mut self, start: i32, op: BinaryOperation, left: AstNode, right: AstNode) -> AstNode {
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::Binary {
                operation: op,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    fn parse_logical_or(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_logical_and();
        while self.consume_optional_operator("||") {
            let right = self.parse_logical_and();
            result = self.make_binary(start, BinaryOperation::Or, result, right);
        }
        result
    }

    fn parse_logical_and(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_nullish_coalescing();
        while self.consume_optional_operator("&&") {
            let right = self.parse_nullish_coalescing();
            result = self.make_binary(start, BinaryOperation::And, result, right);
        }
        result
    }

    fn parse_nullish_coalescing(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_equality();
        while self.consume_optional_operator("??") {
            let right = self.parse_equality();
            result = self.make_binary(start, BinaryOperation::Nullish, result, right);
        }
        result
    }

    /// True iff the next token is an operator whose strValue equals `op`.
    fn next_operator_str(&self) -> Option<String> {
        if let TokenValue::Operator(s) = &self.next().kind {
            Some(s.clone())
        } else {
            None
        }
    }

    fn parse_equality(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_relational();
        while self.next().token_type() == TokenType::Operator {
            let operator = match self.next_operator_str() {
                Some(s) => s,
                None => break,
            };
            match operator.as_str() {
                "==" | "===" | "!=" | "!==" => {
                    self.advance();
                    let right = self.parse_relational();
                    let op = BinaryOperation::from_str(&operator).unwrap();
                    result = self.make_binary(start, op, result, right);
                    continue;
                }
                _ => break,
            }
        }
        result
    }

    fn parse_relational(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_additive();
        while self.next().token_type() == TokenType::Operator
            || self.next().is_keyword_in()
            || self.next().is_keyword_instance_of()
        {
            // strValue for `in`/`instanceof` is the keyword text.
            let operator = self
                .next_operator_str()
                .or_else(|| self.next().to_string_value())
                .unwrap_or_default();
            match operator.as_str() {
                "<" | ">" | "<=" | ">=" | "in" | "instanceof" => {
                    self.advance();
                    let right = self.parse_additive();
                    let op = BinaryOperation::from_str(&operator).unwrap();
                    result = self.make_binary(start, op, result, right);
                    continue;
                }
                _ => break,
            }
        }
        result
    }

    fn parse_additive(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_multiplicative();
        while self.next().token_type() == TokenType::Operator {
            let operator = match self.next_operator_str() {
                Some(s) => s,
                None => break,
            };
            match operator.as_str() {
                "+" | "-" => {
                    self.advance();
                    let right = self.parse_multiplicative();
                    let op = BinaryOperation::from_str(&operator).unwrap();
                    result = self.make_binary(start, op, result, right);
                    continue;
                }
                _ => break,
            }
        }
        result
    }

    fn parse_multiplicative(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_exponentiation();
        while self.next().token_type() == TokenType::Operator {
            let operator = match self.next_operator_str() {
                Some(s) => s,
                None => break,
            };
            match operator.as_str() {
                "*" | "%" | "/" => {
                    self.advance();
                    let right = self.parse_exponentiation();
                    let op = BinaryOperation::from_str(&operator).unwrap();
                    result = self.make_binary(start, op, result, right);
                    continue;
                }
                _ => break,
            }
        }
        result
    }

    fn parse_exponentiation(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_prefix();
        while self.next().token_type() == TokenType::Operator
            && self.next().is_operator("**")
        {
            // Disallow a unary/!/typeof/void operand directly left of `**`.
            if matches!(
                result.kind,
                ExprKind::Unary { .. }
                    | ExprKind::PrefixNot { .. }
                    | ExprKind::TypeofExpression { .. }
                    | ExprKind::VoidExpression { .. }
            ) {
                self.error(
                    "Unary operator used immediately before exponentiation expression. Parenthesis must be used to disambiguate operator precedence",
                    None,
                );
            }
            self.advance();
            let right = self.parse_exponentiation();
            result = self.make_binary(start, BinaryOperation::Pow, result, right);
        }
        result
    }

    fn parse_prefix(&mut self) -> AstNode {
        if self.next().token_type() == TokenType::Operator {
            let start = self.input_index();
            let operator = self.next_operator_str().unwrap_or_default();
            match operator.as_str() {
                "+" => {
                    self.advance();
                    let result = self.parse_prefix();
                    let span = self.span(start, None);
                    let source_span = self.source_span(start, None);
                    return AstNode::create_plus(span, source_span, result);
                }
                "-" => {
                    self.advance();
                    let result = self.parse_prefix();
                    let span = self.span(start, None);
                    let source_span = self.source_span(start, None);
                    return AstNode::create_minus(span, source_span, result);
                }
                "!" => {
                    self.advance();
                    let result = self.parse_prefix();
                    let span = self.span(start, None);
                    let source_span = self.source_span(start, None);
                    return AstNode::new(
                        span,
                        source_span,
                        ExprKind::PrefixNot {
                            expression: Box::new(result),
                        },
                    );
                }
                _ => {}
            }
        } else if self.next().is_keyword_typeof() {
            let start = self.input_index();
            self.advance();
            let result = self.parse_prefix();
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(
                span,
                source_span,
                ExprKind::TypeofExpression {
                    expression: Box::new(result),
                },
            );
        } else if self.next().is_keyword_void() {
            let start = self.input_index();
            self.advance();
            let result = self.parse_prefix();
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(
                span,
                source_span,
                ExprKind::VoidExpression {
                    expression: Box::new(result),
                },
            );
        }
        self.parse_call_chain()
    }

    fn parse_call_chain(&mut self) -> AstNode {
        let start = self.input_index();
        let mut result = self.parse_primary();
        loop {
            if self.consume_optional_character(chars::PERIOD) {
                result = self.parse_access_member(result, start, false);
            } else if self.consume_optional_operator("?.") {
                if self.consume_optional_character(chars::LPAREN) {
                    result = self.parse_call(result, start, true);
                } else if self.consume_optional_character(chars::LBRACKET) {
                    result = self.parse_keyed_read_or_write(result, start, true);
                } else {
                    result = self.parse_access_member(result, start, true);
                }
            } else if self.consume_optional_character(chars::LBRACKET) {
                result = self.parse_keyed_read_or_write(result, start, false);
            } else if self.consume_optional_character(chars::LPAREN) {
                result = self.parse_call(result, start, false);
            } else if self.consume_optional_operator("!") {
                let span = self.span(start, None);
                let source_span = self.source_span(start, None);
                result = AstNode::new(
                    span,
                    source_span,
                    ExprKind::NonNullAssert {
                        expression: Box::new(result),
                    },
                );
            } else if self.next().is_template_literal_end() {
                result = self.parse_no_interpolation_tagged_template_literal(result, start);
            } else if self.next().is_template_literal_part() {
                result = self.parse_tagged_template_literal(result, start);
            } else {
                return result;
            }
        }
    }

    fn parse_primary(&mut self) -> AstNode {
        let start = self.input_index();
        if self.is_arrow_function() {
            return self.parse_arrow_function(start);
        } else if self.consume_optional_character(chars::LPAREN) {
            self.rparens_expected += 1;
            let result = self.parse_pipe();
            if !self.consume_optional_character(chars::RPAREN) {
                self.error("Missing closing parentheses", None);
                self.consume_optional_character(chars::RPAREN);
            }
            self.rparens_expected -= 1;
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(
                span,
                source_span,
                ExprKind::ParenthesizedExpression {
                    expression: Box::new(result),
                },
            );
        } else if self.next().is_keyword_null() {
            self.advance();
            return self.lit_primitive(start, LiteralValue::Null);
        } else if self.next().is_keyword_undefined() {
            self.advance();
            return self.lit_primitive(start, LiteralValue::Undefined);
        } else if self.next().is_keyword_true() {
            self.advance();
            return self.lit_primitive(start, LiteralValue::Bool(true));
        } else if self.next().is_keyword_false() {
            self.advance();
            return self.lit_primitive(start, LiteralValue::Bool(false));
        } else if self.next().is_keyword_this() {
            self.advance();
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(span, source_span, ExprKind::ThisReceiver);
        } else if self.consume_optional_character(chars::LBRACKET) {
            return self.parse_literal_array(start);
        } else if self.next().is_character(chars::LBRACE) {
            return self.parse_literal_map();
        } else if self.next().is_identifier() {
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            let receiver = AstNode::new(span, source_span, ExprKind::ImplicitReceiver);
            return self.parse_access_member(receiver, start, false);
        } else if self.next().is_number() {
            let value = self.next().to_number();
            self.advance();
            return self.lit_primitive(start, LiteralValue::Num(value));
        } else if self.next().is_template_literal_end() {
            return self.parse_no_interpolation_template_literal();
        } else if self.next().is_template_literal_part() {
            return self.parse_template_literal();
        } else if self.next().is_string()
            && matches!(self.next().kind, TokenValue::Str { kind: StringTokenKind::Plain, .. })
        {
            let literal_value = self.next().to_string_value().unwrap_or_default();
            self.advance();
            return self.lit_primitive(start, LiteralValue::Str(literal_value));
        } else if self.next().is_private_identifier() {
            let tok = self.next().clone();
            self.report_error_for_private_identifier(&tok, None);
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(span, source_span, ExprKind::EmptyExpr);
        } else if self.next().is_reg_exp_body() {
            return self.parse_regular_expression_literal();
        } else if self.index >= self.tokens.len() {
            self.error(&format!("Unexpected end of expression: {}", self.input), None);
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(span, source_span, ExprKind::EmptyExpr);
        } else {
            let tok = self.next().to_string_value().unwrap_or_default();
            self.error(&format!("Unexpected token {tok}"), None);
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            return AstNode::new(span, source_span, ExprKind::EmptyExpr);
        }
    }

    fn lit_primitive(&mut self, start: i32, value: LiteralValue) -> AstNode {
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(span, source_span, ExprKind::LiteralPrimitive { value })
    }

    fn parse_literal_array(&mut self, array_start: i32) -> AstNode {
        self.rbrackets_expected += 1;
        let mut elements: Vec<AstNode> = Vec::new();

        loop {
            if self.next().is_operator("...") {
                elements.push(self.parse_spread_element());
            } else if !self.next().is_character(chars::RBRACKET) {
                elements.push(self.parse_pipe());
            } else {
                break;
            }
            if !self.consume_optional_character(chars::COMMA) {
                break;
            }
        }

        self.rbrackets_expected -= 1;
        self.expect_character(chars::RBRACKET);
        let span = self.span(array_start, None);
        let source_span = self.source_span(array_start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::LiteralArray {
                expressions: elements,
            },
        )
    }

    fn parse_literal_map(&mut self) -> AstNode {
        let mut keys: Vec<LiteralMapKey> = Vec::new();
        let mut values: Vec<AstNode> = Vec::new();
        let start = self.input_index();
        self.expect_character(chars::LBRACE);
        if !self.consume_optional_character(chars::RBRACE) {
            self.rbraces_expected += 1;
            loop {
                let key_start = self.input_index();

                if self.next().is_operator("...") {
                    self.advance();
                    let span = self.span(key_start, None);
                    let source_span = self.source_span(key_start, None);
                    keys.push(LiteralMapKey::Spread { span, source_span });
                    values.push(self.parse_pipe());
                } else {
                    let quoted = self.next().is_string();
                    let key = self.expect_identifier_or_keyword_or_string();
                    let key_span = self.span(key_start, None);
                    let key_source_span = self.source_span(key_start, None);

                    if quoted {
                        keys.push(LiteralMapKey::Property {
                            key,
                            quoted,
                            span: key_span,
                            source_span: key_source_span,
                            is_shorthand_initialized: false,
                        });
                        self.expect_character(chars::COLON);
                        values.push(self.parse_pipe());
                    } else if self.consume_optional_character(chars::COLON) {
                        keys.push(LiteralMapKey::Property {
                            key,
                            quoted,
                            span: key_span,
                            source_span: key_source_span,
                            is_shorthand_initialized: false,
                        });
                        values.push(self.parse_pipe());
                    } else {
                        keys.push(LiteralMapKey::Property {
                            key: key.clone(),
                            quoted,
                            span: key_span,
                            source_span: key_source_span,
                            is_shorthand_initialized: true,
                        });
                        let receiver =
                            AstNode::new(key_span, key_source_span, ExprKind::ImplicitReceiver);
                        values.push(AstNode::new(
                            key_span,
                            key_source_span,
                            ExprKind::PropertyRead {
                                name_span: key_source_span,
                                receiver: Box::new(receiver),
                                name: key,
                            },
                        ));
                    }
                }

                if !(self.consume_optional_character(chars::COMMA)
                    && !self.next().is_character(chars::RBRACE))
                {
                    break;
                }
            }
            self.rbraces_expected -= 1;
            self.expect_character(chars::RBRACE);
        }
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(span, source_span, ExprKind::LiteralMap { keys, values })
    }

    fn parse_access_member(&mut self, read_receiver: AstNode, start: i32, is_safe: bool) -> AstNode {
        let name_start = self.input_index();
        // withContext(Writable, ...): mirror TS `context |= WRITABLE; ...; context ^= WRITABLE`.
        self.context = ParseContextFlags(self.context.0 | ParseContextFlags::WRITABLE.0);
        let id = {
            let id = self.expect_identifier_or_keyword().unwrap_or_default();
            if id.is_empty() {
                let end = read_receiver.span.end as i32;
                self.error("Expected identifier for property access", Some(end));
            }
            id
        };
        self.context = ParseContextFlags(self.context.0 ^ ParseContextFlags::WRITABLE.0);
        let name_span = self.source_span(name_start, None);

        if is_safe {
            if Self::is_assignment_operator(self.next()) {
                self.advance();
                self.error("The '?.' operator cannot be used in the assignment", None);
                let span = self.span(start, None);
                let source_span = self.source_span(start, None);
                AstNode::new(span, source_span, ExprKind::EmptyExpr)
            } else {
                let span = self.span(start, None);
                let source_span = self.source_span(start, None);
                AstNode::new(
                    span,
                    source_span,
                    ExprKind::SafePropertyRead {
                        name_span,
                        receiver: Box::new(read_receiver),
                        name: id,
                    },
                )
            }
        } else if Self::is_assignment_operator(self.next()) {
            let operation_str = self.next_operator_str().unwrap_or_default();
            if !self.parse_flags.contains_action() {
                self.advance();
                self.error("Bindings cannot contain assignments", None);
                let span = self.span(start, None);
                let source_span = self.source_span(start, None);
                return AstNode::new(span, source_span, ExprKind::EmptyExpr);
            }
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            let receiver = AstNode::new(
                span,
                source_span,
                ExprKind::PropertyRead {
                    name_span,
                    receiver: Box::new(read_receiver),
                    name: id,
                },
            );
            self.advance();
            let value = self.parse_conditional();
            let op = BinaryOperation::from_str(&operation_str).unwrap();
            self.make_binary(start, op, receiver, value)
        } else {
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            AstNode::new(
                span,
                source_span,
                ExprKind::PropertyRead {
                    name_span,
                    receiver: Box::new(read_receiver),
                    name: id,
                },
            )
        }
    }

    fn parse_call(&mut self, receiver: AstNode, start: i32, is_safe: bool) -> AstNode {
        let argument_start = self.input_index();
        self.rparens_expected += 1;
        let args = self.parse_call_arguments();
        let argument_span = self
            .span(argument_start, Some(self.input_index()))
            .to_absolute(self.absolute_offset.max(0) as u32);
        self.expect_character(chars::RPAREN);
        self.rparens_expected -= 1;
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        if is_safe {
            AstNode::new(
                span,
                source_span,
                ExprKind::SafeCall {
                    receiver: Box::new(receiver),
                    args,
                    argument_span,
                },
            )
        } else {
            AstNode::new(
                span,
                source_span,
                ExprKind::Call {
                    receiver: Box::new(receiver),
                    args,
                    argument_span,
                },
            )
        }
    }

    fn parse_call_arguments(&mut self) -> Vec<AstNode> {
        if self.next().is_character(chars::RPAREN) {
            return Vec::new();
        }
        let mut positionals: Vec<AstNode> = Vec::new();
        loop {
            if self.next().is_operator("...") {
                positionals.push(self.parse_spread_element());
            } else {
                positionals.push(self.parse_pipe());
            }
            if !self.consume_optional_character(chars::COMMA) {
                break;
            }
        }
        positionals
    }

    fn parse_spread_element(&mut self) -> AstNode {
        if !self.next().is_operator("...") {
            self.error("Spread element must start with '...' operator", None);
        }
        let spread_start = self.input_index();
        self.advance();
        let expression = self.parse_pipe();
        let span = self.span(spread_start, None);
        let source_span = self.source_span(spread_start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::SpreadElement {
                expression: Box::new(expression),
            },
        )
    }

    /// `expectTemplateBindingKey` — identifier/keyword/string with optional `-`.
    fn expect_template_binding_key(&mut self) -> TemplateBindingIdentifier {
        let mut result = String::new();
        let mut operator_found;
        let start = self.current_absolute_offset();
        loop {
            result.push_str(&self.expect_identifier_or_keyword_or_string());
            operator_found = self.consume_optional_operator("-");
            if operator_found {
                result.push('-');
            }
            if !operator_found {
                break;
            }
        }
        let len = result.encode_utf16().count() as u32;
        TemplateBindingIdentifier {
            source: result,
            span: AbsoluteSourceSpan::new(start.max(0) as u32, start.max(0) as u32 + len),
        }
    }

    fn parse_template_bindings(
        &mut self,
        template_key: TemplateBindingIdentifier,
    ) -> TemplateBindingParseResult {
        let mut bindings: Vec<TemplateBinding> = Vec::new();

        bindings.extend(self.parse_directive_keyword_bindings(&template_key));

        while self.index < self.tokens.len() {
            if let Some(let_binding) = self.parse_let_binding() {
                bindings.push(let_binding);
            } else {
                let mut key = self.expect_template_binding_key();
                if let Some(binding) = self.parse_as_binding(&key) {
                    bindings.push(binding);
                } else {
                    // Transform e.g. `of` -> `ngForOf`.
                    let first_upper = upper_first(&key.source);
                    key.source = format!("{}{}", template_key.source, first_upper);
                    bindings.extend(self.parse_directive_keyword_bindings(&key));
                }
            }
            self.consume_statement_terminator();
        }

        let errors = std::mem::take(self.errors);
        TemplateBindingParseResult {
            template_bindings: bindings,
            warnings: Vec::new(),
            errors,
        }
    }

    fn parse_keyed_read_or_write(
        &mut self,
        receiver: AstNode,
        start: i32,
        is_safe: bool,
    ) -> AstNode {
        // withContext(Writable, ...)
        self.context = ParseContextFlags(self.context.0 | ParseContextFlags::WRITABLE.0);

        let result = (|| {
            self.rbrackets_expected += 1;
            let key = self.parse_pipe();
            if matches!(key.kind, ExprKind::EmptyExpr) {
                self.error("Key access cannot be empty", None);
            }
            self.rbrackets_expected -= 1;
            self.expect_character(chars::RBRACKET);
            if Self::is_assignment_operator(self.next()) {
                let operation_str = self.next_operator_str().unwrap_or_default();
                if is_safe {
                    self.advance();
                    self.error("The '?.' operator cannot be used in the assignment", None);
                } else {
                    let span = self.span(start, None);
                    let source_span = self.source_span(start, None);
                    let binary_receiver = AstNode::new(
                        span,
                        source_span,
                        ExprKind::KeyedRead {
                            receiver: Box::new(receiver),
                            key: Box::new(key),
                        },
                    );
                    self.advance();
                    let value = self.parse_conditional();
                    let op = BinaryOperation::from_str(&operation_str).unwrap();
                    return self.make_binary(start, op, binary_receiver, value);
                }
            } else {
                let span = self.span(start, None);
                let source_span = self.source_span(start, None);
                return if is_safe {
                    AstNode::new(
                        span,
                        source_span,
                        ExprKind::SafeKeyedRead {
                            receiver: Box::new(receiver),
                            key: Box::new(key),
                        },
                    )
                } else {
                    AstNode::new(
                        span,
                        source_span,
                        ExprKind::KeyedRead {
                            receiver: Box::new(receiver),
                            key: Box::new(key),
                        },
                    )
                };
            }
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            AstNode::new(span, source_span, ExprKind::EmptyExpr)
        })();

        self.context = ParseContextFlags(self.context.0 ^ ParseContextFlags::WRITABLE.0);
        result
    }

    fn parse_directive_keyword_bindings(
        &mut self,
        key: &TemplateBindingIdentifier,
    ) -> Vec<TemplateBinding> {
        let mut bindings: Vec<TemplateBinding> = Vec::new();
        self.consume_optional_character(chars::COLON);
        let value = self.get_directive_bound_target();
        let mut span_end = self.current_absolute_offset();
        let as_binding = self.parse_as_binding(key);
        if as_binding.is_none() {
            self.consume_statement_terminator();
            span_end = self.current_absolute_offset();
        }
        let source_span =
            AbsoluteSourceSpan::new(key.span.start, span_end.max(0) as u32);
        bindings.push(TemplateBinding::Expression {
            source_span,
            key: key.clone(),
            value,
        });
        if let Some(b) = as_binding {
            bindings.push(b);
        }
        bindings
    }

    fn get_directive_bound_target(&mut self) -> Option<AstWithSource> {
        if (self.next().index == -1 && self.next().end == -1)
            || self.peek_keyword_as()
            || self.peek_keyword_let()
        {
            return None;
        }
        let ast = self.parse_pipe();
        let start = ast.span.start;
        let end = ast.span.end;
        let units: Vec<u16> = self.input.encode_utf16().collect();
        let value = utf16_substring(&units, start as usize, end as usize);
        // ASTWithSource clones the shared errors here; we keep a snapshot.
        let errors = self.errors.clone();
        Some(AstWithSource::new(
            ast,
            Some(value),
            get_location(&self.parse_source_span),
            (self.absolute_offset + start as i32).max(0) as u32,
            errors,
        ))
    }

    fn parse_as_binding(&mut self, value: &TemplateBindingIdentifier) -> Option<TemplateBinding> {
        if !self.peek_keyword_as() {
            return None;
        }
        self.advance();
        let key = self.expect_template_binding_key();
        self.consume_statement_terminator();
        let source_span =
            AbsoluteSourceSpan::new(value.span.start, self.current_absolute_offset().max(0) as u32);
        Some(TemplateBinding::Variable {
            source_span,
            key,
            value: Some(value.clone()),
        })
    }

    fn parse_let_binding(&mut self) -> Option<TemplateBinding> {
        if !self.peek_keyword_let() {
            return None;
        }
        let span_start = self.current_absolute_offset();
        self.advance();
        let key = self.expect_template_binding_key();
        let mut value: Option<TemplateBindingIdentifier> = None;
        if self.consume_optional_operator("=") {
            value = Some(self.expect_template_binding_key());
        }
        self.consume_statement_terminator();
        let source_span =
            AbsoluteSourceSpan::new(span_start.max(0) as u32, self.current_absolute_offset().max(0) as u32);
        Some(TemplateBinding::Variable {
            source_span,
            key,
            value,
        })
    }

    fn parse_no_interpolation_tagged_template_literal(&mut self, tag: AstNode, start: i32) -> AstNode {
        let template = self.parse_no_interpolation_template_literal();
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::TaggedTemplateLiteral {
                tag: Box::new(tag),
                template: Box::new(template),
            },
        )
    }

    fn parse_no_interpolation_template_literal(&mut self) -> AstNode {
        let text = self.next().to_string_value().unwrap_or_default();
        let start = self.input_index();
        self.advance();
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::TemplateLiteral {
                elements: vec![TemplateLiteralElement {
                    span,
                    source_span,
                    text,
                }],
                expressions: Vec::new(),
            },
        )
    }

    fn parse_tagged_template_literal(&mut self, tag: AstNode, start: i32) -> AstNode {
        let template = self.parse_template_literal();
        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::TaggedTemplateLiteral {
                tag: Box::new(tag),
                template: Box::new(template),
            },
        )
    }

    fn parse_template_literal(&mut self) -> AstNode {
        let mut elements: Vec<TemplateLiteralElement> = Vec::new();
        let mut expressions: Vec<AstNode> = Vec::new();
        let start = self.input_index();

        while !(self.next().index == -1 && self.next().end == -1) {
            let is_part = self.next().is_template_literal_part();
            let is_end = self.next().is_template_literal_end();
            let is_interp_start = self.next().is_template_literal_interpolation_start();

            if is_part || is_end {
                let part_start = self.input_index();
                let text = self.next().to_string_value().unwrap_or_default();
                self.advance();
                let span = self.span(part_start, None);
                let source_span = self.source_span(part_start, None);
                elements.push(TemplateLiteralElement {
                    span,
                    source_span,
                    text,
                });
                if is_end {
                    break;
                }
            } else if is_interp_start {
                self.advance();
                self.rbraces_expected += 1;
                let expression = self.parse_pipe();
                if matches!(expression.kind, ExprKind::EmptyExpr) {
                    self.error("Template literal interpolation cannot be empty", None);
                } else {
                    expressions.push(expression);
                }
                self.rbraces_expected -= 1;
            } else {
                self.advance();
            }
        }

        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::TemplateLiteral {
                elements,
                expressions,
            },
        )
    }

    fn parse_regular_expression_literal(&mut self) -> AstNode {
        let body_token = self.next().clone();
        self.advance();

        if !body_token.is_reg_exp_body() {
            let ii = self.input_index();
            let span = self.span(ii, None);
            let source_span = self.source_span(ii, None);
            return AstNode::new(span, source_span, ExprKind::EmptyExpr);
        }

        let mut flags_token: Option<Token> = None;

        if self.next().is_reg_exp_flags() {
            let ft = self.next().clone();
            self.advance();
            let flags_str = ft.to_string_value().unwrap_or_default();
            let mut seen_flags: Vec<char> = Vec::new();
            for (i, ch) in flags_str.chars().enumerate() {
                if !SUPPORTED_REGEX_FLAGS.contains(&ch) {
                    let supported = SUPPORTED_REGEX_FLAGS
                        .iter()
                        .map(|f| format!("\"{f}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.error(
                        &format!(
                            "Unsupported regular expression flag \"{ch}\". The supported flags are: {supported}"
                        ),
                        Some(ft.index + i as i32),
                    );
                } else if seen_flags.contains(&ch) {
                    self.error(
                        &format!("Duplicate regular expression flag \"{ch}\""),
                        Some(ft.index + i as i32),
                    );
                } else {
                    seen_flags.push(ch);
                }
            }
            flags_token = Some(ft);
        }

        let start = body_token.index;
        let end = match &flags_token {
            Some(ft) => ft.end,
            None => body_token.end,
        };

        let span = self.span(start, Some(end));
        let source_span = self.source_span(start, Some(end));
        let body = match &body_token.kind {
            TokenValue::RegExpBody(s) => s.clone(),
            _ => String::new(),
        };
        let flags = flags_token.map(|ft| ft.to_string_value().unwrap_or_default());
        AstNode::new(
            span,
            source_span,
            ExprKind::RegularExpressionLiteral { body, flags },
        )
    }

    fn parse_arrow_function(&mut self, start: i32) -> AstNode {
        let params: Vec<ArrowFunctionParameter>;

        if self.next().is_identifier() {
            let token = self.next().clone();
            self.advance();
            params = vec![self.get_arrow_function_identifier_arg(&token)];
        } else if self.next().is_character(chars::LPAREN) {
            self.rparens_expected += 1;
            self.advance();
            params = self.parse_arrow_function_parameters();
            self.rparens_expected -= 1;
        } else {
            let tok = self.next().to_string_value().unwrap_or_default();
            self.error(&format!("Unexpected token {tok}"), None);
            params = Vec::new();
        }

        self.expect_operator("=>");
        let body: AstNode;

        if self.next().is_character(chars::LBRACE) {
            self.error(
                "Multi-line arrow functions are not supported. If you meant to return an object literal, wrap it with parentheses.",
                None,
            );
            let span = self.span(start, None);
            let source_span = self.source_span(start, None);
            body = AstNode::new(span, source_span, ExprKind::EmptyExpr);
        } else {
            let prev_flags = self.parse_flags;
            self.parse_flags = ParseFlags::ACTION;
            body = self.parse_expression();
            self.parse_flags = prev_flags;
        }

        let span = self.span(start, None);
        let source_span = self.source_span(start, None);
        AstNode::new(
            span,
            source_span,
            ExprKind::ArrowFunction {
                parameters: params,
                body: Box::new(body),
            },
        )
    }

    fn parse_arrow_function_parameters(&mut self) -> Vec<ArrowFunctionParameter> {
        let mut params: Vec<ArrowFunctionParameter> = Vec::new();

        if !self.consume_optional_character(chars::RPAREN) {
            while !(self.next().index == -1 && self.next().end == -1) {
                if self.next().is_identifier() {
                    let token = self.next().clone();
                    self.advance();
                    params.push(self.get_arrow_function_identifier_arg(&token));

                    if self.consume_optional_character(chars::RPAREN) {
                        break;
                    } else {
                        self.expect_character(chars::COMMA);
                    }
                } else {
                    let tok = self.next().to_string_value().unwrap_or_default();
                    self.error(&format!("Unexpected token {tok}"), None);
                    break;
                }
            }
        }

        params
    }

    fn get_arrow_function_identifier_arg(&mut self, token: &Token) -> ArrowFunctionParameter {
        let name = token.to_string_value().unwrap_or_default();
        let span = self.span(token.index, None);
        let source_span = self.source_span(token.index, None);
        ArrowFunctionParameter::Identifier(ArrowFunctionIdentifierParameter {
            name,
            span,
            source_span,
        })
    }

    fn is_arrow_function(&self) -> bool {
        let start = self.index as i32;
        let tokens = &self.tokens;

        if start > tokens.len() as i32 - 2 {
            return false;
        }
        let s = start as usize;

        // One parameter and no parens.
        if tokens[s].is_identifier() && tokens[s + 1].is_operator("=>") {
            return true;
        }

        // Multiple parenthesized params.
        if tokens[s].is_character(chars::LPAREN) {
            let mut i = s + 1;
            while i < tokens.len() {
                if !tokens[i].is_identifier() && !tokens[i].is_character(chars::COMMA) {
                    break;
                }
                i += 1;
            }
            return i < tokens.len() - 1
                && tokens[i].is_character(chars::RPAREN)
                && tokens[i + 1].is_operator("=>");
        }

        false
    }

    /// Consume the optional statement terminator: semicolon or comma.
    fn consume_statement_terminator(&mut self) {
        if !self.consume_optional_character(chars::SEMICOLON) {
            self.consume_optional_character(chars::COMMA);
        }
    }

    fn error(&mut self, message: &str, index: Option<i32>) {
        let index = index.unwrap_or(self.index as i32);
        let location_text = self.get_error_location_text(index);
        let err = get_parse_error(message, &self.input, &location_text, &self.parse_source_span);
        self.errors.push(err);
        self.skip();
    }

    fn get_error_location_text(&self, index: i32) -> String {
        if index >= 0 && (index as usize) < self.tokens.len() {
            format!("at column {} in", self.tokens[index as usize].index + 1)
        } else {
            "at the end of the expression".to_string()
        }
    }

    fn report_error_for_private_identifier(&mut self, token: &Token, extra_message: Option<&str>) {
        let tok_text = token.to_string_value().unwrap_or_default();
        let mut error_message =
            format!("Private identifiers are not supported. Unexpected private identifier: {tok_text}");
        if let Some(extra) = extra_message {
            error_message += &format!(", {extra}");
        }
        self.error(&error_message, None);
    }

    /// `skip()` — error recovery. See module docs / source comment for the
    /// recovery-point rules.
    fn skip(&mut self) {
        loop {
            if self.index >= self.tokens.len() {
                break;
            }
            let n = self.next();
            let stop = n.is_character(chars::SEMICOLON)
                || n.is_operator("|")
                || (self.rparens_expected > 0 && n.is_character(chars::RPAREN))
                || (self.rbraces_expected > 0 && n.is_character(chars::RBRACE))
                || (self.rbrackets_expected > 0 && n.is_character(chars::RBRACKET))
                || (self.context.is_writable() && Self::is_assignment_operator(n));
            if stop {
                break;
            }
            if self.next().is_error() {
                let msg = self.next().to_string_value().unwrap_or_default();
                let idx = self.next().index;
                let location_text = self.get_error_location_text(idx);
                let err =
                    get_parse_error(&msg, &self.input, &location_text, &self.parse_source_span);
                self.errors.push(err);
            }
            self.advance();
        }
    }
}

// ---------------------------------------------------------------------------
// SimpleExpressionChecker — rejects pipes (host bindings).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct SimpleExpressionChecker {
    errors: Vec<String>,
}

impl super::ast::AstVisitor for SimpleExpressionChecker {
    fn visit_pipe(&mut self, _node: &AstNode) {
        self.errors.push("pipes".to_string());
    }
}

// ---------------------------------------------------------------------------
// UTF-16 helpers (the parser/lexer work in UTF-16 code units).
// ---------------------------------------------------------------------------

/// Decode `units[start..end]` (clamped) into a `String`.
fn utf16_substring(units: &[u16], start: usize, end: usize) -> String {
    let start = start.min(units.len());
    let end = end.min(units.len());
    if start >= end {
        return String::new();
    }
    String::from_utf16_lossy(&units[start..end])
}

/// First index >= `from` where `needle` occurs in `haystack`, else None.
fn index_of(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(haystack.len()));
    }
    if from >= haystack.len() {
        return None;
    }
    let last = haystack.len().checked_sub(needle.len())?;
    (from..=last).find(|&i| &haystack[i..i + needle.len()] == needle)
}

/// Whether `haystack` starts with `needle` at index `at`.
fn starts_with_at(haystack: &[u16], needle: &[u16], at: usize) -> bool {
    if at + needle.len() > haystack.len() {
        return false;
    }
    &haystack[at..at + needle.len()] == needle
}

/// Uppercase the first character (`s.charAt(0).toUpperCase() + s.substring(1)`).
fn upper_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn pss() -> ParseSourceSpan {
        ParseSourceSpan { start: 0, end: 0 }
    }

    fn parse_binding(input: &str) -> AstWithSource {
        Parser::default().parse_binding(input, pss(), 0)
    }

    fn parse_action(input: &str) -> AstWithSource {
        Parser::default().parse_action(input, pss(), 0)
    }

    #[test]
    fn parses_simple_property_read() {
        let r = parse_binding("a.b");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::PropertyRead { name, receiver, .. } => {
                assert_eq!(name, "b");
                assert!(matches!(receiver.kind, ExprKind::PropertyRead { .. }));
            }
            other => panic!("expected PropertyRead, got {other:?}"),
        }
    }

    #[test]
    fn parses_literal_number() {
        let r = parse_binding("42");
        match &r.ast.kind {
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Num(n),
            } => assert_eq!(*n, 42.0),
            other => panic!("expected number literal, got {other:?}"),
        }
    }

    #[test]
    fn precedence_add_then_mul() {
        // a + b * c  =>  Binary(+, a, Binary(*, b, c))
        let r = parse_binding("a + b * c");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::Binary {
                operation: BinaryOperation::Add,
                right,
                ..
            } => {
                assert!(matches!(
                    right.kind,
                    ExprKind::Binary {
                        operation: BinaryOperation::Mul,
                        ..
                    }
                ));
            }
            other => panic!("expected Binary(+), got {other:?}"),
        }
    }

    #[test]
    fn exponentiation_right_assoc() {
        // a ** b ** c => Binary(**, a, Binary(**, b, c))
        let r = parse_binding("a ** b ** c");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::Binary {
                operation: BinaryOperation::Pow,
                right,
                ..
            } => assert!(matches!(
                right.kind,
                ExprKind::Binary {
                    operation: BinaryOperation::Pow,
                    ..
                }
            )),
            other => panic!("expected Binary(**), got {other:?}"),
        }
    }

    #[test]
    fn unary_before_exponentiation_is_error() {
        let r = parse_binding("-a ** b");
        assert!(!r.errors.is_empty());
        assert!(r.errors[0].msg.contains("Unary operator used immediately before"));
    }

    #[test]
    fn conditional_ternary() {
        let r = parse_binding("a ? b : c");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(r.ast.kind, ExprKind::Conditional { .. }));
    }

    #[test]
    fn ternary_missing_branch_errors() {
        let r = parse_binding("a ? b");
        assert!(!r.errors.is_empty());
        assert!(r.errors[0].msg.contains("requires all 3 expressions"));
    }

    #[test]
    fn pipe_with_args() {
        let r = parse_binding("a | slice:1:2");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::BindingPipe { name, args, .. } => {
                assert_eq!(name, "slice");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected BindingPipe, got {other:?}"),
        }
    }

    #[test]
    fn pipe_in_action_is_error() {
        let r = parse_action("a | b");
        assert!(!r.errors.is_empty());
        assert!(r.errors.iter().any(|e| e.msg.contains("pipe in an action")));
    }

    #[test]
    fn assignment_in_binding_is_error() {
        let r = parse_binding("a = b");
        assert!(!r.errors.is_empty());
        assert!(r.errors.iter().any(|e| e.msg.contains("Bindings cannot contain assignments")));
    }

    #[test]
    fn assignment_in_action_is_binary() {
        let r = parse_action("a = b");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(
            r.ast.kind,
            ExprKind::Binary {
                operation: BinaryOperation::Assignment(_),
                ..
            }
        ));
    }

    #[test]
    fn chain_in_action() {
        let r = parse_action("a(); b()");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::Chain { expressions } => assert_eq!(expressions.len(), 2),
            other => panic!("expected Chain, got {other:?}"),
        }
    }

    #[test]
    fn chain_in_binding_is_error() {
        let r = parse_binding("a; b");
        assert!(!r.errors.is_empty());
        assert!(r
            .errors
            .iter()
            .any(|e| e.msg.contains("cannot contain chained expression")));
    }

    #[test]
    fn safe_navigation() {
        let r = parse_binding("a?.b");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(r.ast.kind, ExprKind::SafePropertyRead { .. }));
    }

    #[test]
    fn keyed_read() {
        let r = parse_binding("a[b]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(r.ast.kind, ExprKind::KeyedRead { .. }));
    }

    #[test]
    fn call_with_args() {
        let r = parse_binding("f(1, 2)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::Call { args, .. } => assert_eq!(args.len(), 2),
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn literal_array() {
        let r = parse_binding("[1, 2, 3]");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::LiteralArray { expressions } => assert_eq!(expressions.len(), 3),
            other => panic!("expected LiteralArray, got {other:?}"),
        }
    }

    #[test]
    fn literal_map_with_shorthand() {
        let r = parse_binding("{a, b: 2}");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::LiteralMap { keys, values } => {
                assert_eq!(keys.len(), 2);
                assert_eq!(values.len(), 2);
                match &keys[0] {
                    LiteralMapKey::Property {
                        is_shorthand_initialized,
                        ..
                    } => assert!(*is_shorthand_initialized),
                    _ => panic!("expected property key"),
                }
                // shorthand value is a PropertyRead on ImplicitReceiver
                assert!(matches!(values[0].kind, ExprKind::PropertyRead { .. }));
            }
            other => panic!("expected LiteralMap, got {other:?}"),
        }
    }

    #[test]
    fn prefix_not_and_typeof_void() {
        assert!(matches!(parse_binding("!a").ast.kind, ExprKind::PrefixNot { .. }));
        assert!(matches!(
            parse_binding("typeof a").ast.kind,
            ExprKind::TypeofExpression { .. }
        ));
        assert!(matches!(
            parse_binding("void a").ast.kind,
            ExprKind::VoidExpression { .. }
        ));
    }

    #[test]
    fn unary_minus_is_unary_node() {
        let r = parse_binding("-a");
        assert!(matches!(
            r.ast.kind,
            ExprKind::Unary {
                operator: super::super::ast::UnaryOperator::Minus,
                ..
            }
        ));
    }

    #[test]
    fn non_null_assert() {
        let r = parse_binding("a!");
        assert!(matches!(r.ast.kind, ExprKind::NonNullAssert { .. }));
    }

    #[test]
    fn parenthesized() {
        let r = parse_binding("(a + b)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(r.ast.kind, ExprKind::ParenthesizedExpression { .. }));
    }

    #[test]
    fn this_receiver() {
        let r = parse_binding("this");
        assert!(matches!(r.ast.kind, ExprKind::ThisReceiver));
    }

    #[test]
    fn null_undefined_distinct() {
        assert!(matches!(
            parse_binding("null").ast.kind,
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Null
            }
        ));
        assert!(matches!(
            parse_binding("undefined").ast.kind,
            ExprKind::LiteralPrimitive {
                value: LiteralValue::Undefined
            }
        ));
    }

    #[test]
    fn empty_input_is_empty_expr() {
        let r = parse_binding("");
        assert!(matches!(r.ast.kind, ExprKind::EmptyExpr));
    }

    #[test]
    fn arrow_function_single_param() {
        let r = parse_binding("a => a + 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::ArrowFunction { parameters, .. } => assert_eq!(parameters.len(), 1),
            other => panic!("expected ArrowFunction, got {other:?}"),
        }
    }

    #[test]
    fn arrow_function_multi_param() {
        let r = parse_binding("(a, b) => a + b");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::ArrowFunction { parameters, .. } => assert_eq!(parameters.len(), 2),
            other => panic!("expected ArrowFunction, got {other:?}"),
        }
    }

    #[test]
    fn arrow_body_allows_assignment() {
        let r = parse_binding("() => a = 1");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
    }

    #[test]
    fn interpolation_basic() {
        let p = Parser::default();
        let r = p.parse_interpolation("{{ a }}{{ b }}", pss(), 0);
        let r = r.expect("should produce interpolation");
        match &r.ast.kind {
            ExprKind::Interpolation {
                strings,
                expressions,
            } => {
                assert_eq!(expressions.len(), 2);
                assert_eq!(strings.len(), 3);
            }
            other => panic!("expected Interpolation, got {other:?}"),
        }
    }

    #[test]
    fn interpolation_none_when_no_braces() {
        let p = Parser::default();
        assert!(p.parse_interpolation("plain text", pss(), 0).is_none());
    }

    #[test]
    fn interpolation_blank_is_error() {
        let p = Parser::default();
        let mut errors = Vec::new();
        let split = p.split_interpolation("{{ }}", &pss(), &mut errors);
        assert_eq!(split.expressions.len(), 1);
        assert!(!errors.is_empty());
        assert!(errors[0].msg.contains("Blank expressions"));
    }

    #[test]
    fn strip_comments_quote_aware() {
        let p = Parser::default();
        let (stripped, has) = p.strip_comments("a + b // trailing");
        assert!(has);
        assert_eq!(stripped.trim(), "a + b");
        // // inside string is not a comment
        let (stripped2, has2) = p.strip_comments("'a//b'");
        assert!(!has2);
        assert_eq!(stripped2, "'a//b'");
    }

    #[test]
    fn check_no_interpolation_error() {
        let r = parse_binding("{{ a }}");
        assert!(r.errors.iter().any(|e| e.msg.contains("Got interpolation")));
    }

    #[test]
    fn simple_binding_rejects_pipes() {
        let p = Parser::default();
        let r = p.parse_simple_binding("a | b", pss(), 0);
        assert!(r
            .errors
            .iter()
            .any(|e| e.msg.contains("Host binding expression cannot contain pipes")));
    }

    #[test]
    fn template_bindings_ngfor() {
        let p = Parser::default();
        let r = p.parse_template_bindings("ngFor", "let item of items", pss(), 0, 0);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        // ngFor -> null, item -> $implicit (variable), ngForOf -> items
        assert!(r.template_bindings.len() >= 3);
        // First is the ngFor expression binding with no value.
        match &r.template_bindings[0] {
            TemplateBinding::Expression { key, value, .. } => {
                assert_eq!(key.source, "ngFor");
                assert!(value.is_none());
            }
            other => panic!("expected Expression binding, got {other:?}"),
        }
    }

    #[test]
    fn template_bindings_as() {
        let p = Parser::default();
        let r = p.parse_template_bindings("ngIf", "cond as c", pss(), 0, 0);
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(r
            .template_bindings
            .iter()
            .any(|b| matches!(b, TemplateBinding::Variable { .. })));
    }

    #[test]
    fn regex_literal() {
        let r = parse_binding("a = /ab+/g");
        // assignment in a binding is an error, but the regex itself should parse
        // inside an action:
        let r2 = parse_action("/ab+/g");
        assert!(r2.errors.is_empty(), "errors: {:?}", r2.errors);
        match &r2.ast.kind {
            ExprKind::RegularExpressionLiteral { body, flags } => {
                assert_eq!(body, "ab+");
                assert_eq!(flags.as_deref(), Some("g"));
            }
            other => panic!("expected RegularExpressionLiteral, got {other:?}"),
        }
        // The `a = ...` form errors (assignment in binding).
        assert!(!r.errors.is_empty());
    }

    #[test]
    fn unsupported_regex_flag_errors() {
        let r = parse_action("/ab+/z");
        assert!(r
            .errors
            .iter()
            .any(|e| e.msg.contains("Unsupported regular expression flag")));
    }

    #[test]
    fn template_literal_with_interpolation() {
        let r = parse_binding("`a${x}b`");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::TemplateLiteral {
                elements,
                expressions,
            } => {
                assert_eq!(elements.len(), 2);
                assert_eq!(expressions.len(), 1);
            }
            other => panic!("expected TemplateLiteral, got {other:?}"),
        }
    }

    #[test]
    fn tagged_template_literal() {
        let r = parse_binding("tag`a${x}b`");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(r.ast.kind, ExprKind::TaggedTemplateLiteral { .. }));
    }

    #[test]
    fn missing_closing_paren_recovers() {
        let r = parse_binding("(a + b");
        assert!(r.errors.iter().any(|e| e.msg.contains("Missing closing parentheses")));
    }

    #[test]
    fn relational_in_operator() {
        let r = parse_binding("'x' in obj");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(
            r.ast.kind,
            ExprKind::Binary {
                operation: BinaryOperation::In,
                ..
            }
        ));
    }

    #[test]
    fn nullish_coalescing() {
        let r = parse_binding("a ?? b");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        assert!(matches!(
            r.ast.kind,
            ExprKind::Binary {
                operation: BinaryOperation::Nullish,
                ..
            }
        ));
    }

    #[test]
    fn spread_in_call() {
        let r = parse_binding("f(...a)");
        assert!(r.errors.is_empty(), "errors: {:?}", r.errors);
        match &r.ast.kind {
            ExprKind::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                assert!(matches!(args[0].kind, ExprKind::SpreadElement { .. }));
            }
            other => panic!("expected Call with spread, got {other:?}"),
        }
    }

    #[test]
    fn interpolation_expression_for_icu() {
        let p = Parser::default();
        let r = p.parse_interpolation_expression("count", pss(), 0);
        match &r.ast.kind {
            ExprKind::Interpolation {
                strings,
                expressions,
            } => {
                assert_eq!(strings, &vec![String::new(), String::new()]);
                assert_eq!(expressions.len(), 1);
            }
            other => panic!("expected Interpolation, got {other:?}"),
        }
    }
}
