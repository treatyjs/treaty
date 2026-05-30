//! HTML/ML parser front-end (tokenizer + parser + HTML AST) — the input to the render3
//! template transform. No render3-study spec; ported directly from Angular source.
//!
//! Source: `tools/angular-ref/packages/compiler/src/ml_parser/` (lexer.ts, parser.ts,
//! ast.ts, html_parser.ts, tags.ts, html_tags.ts, tokens.ts) plus `chars.ts`.
//!
//! This is an owned, arena-free port: the AST uses `Box`/`Vec`/`String`, and spans are
//! cheap owned values ([`ParseSourceSpan`]/[`ParseLocation`]/[`ParseSourceFile`]). Only
//! the plain (non-escaped-string) character cursor is ported; templates are not stored in
//! escaped JS strings in this pipeline. ICU expansion forms and `@`-control-flow blocks are
//! tokenized/parsed here, exactly as in Angular 17+.

use std::collections::HashMap;
use std::rc::Rc;

// ---------------------------------------------------------------------------
// Spans (port of `parse_util.ts`: ParseSourceFile / ParseLocation / ParseSourceSpan /
// ParseError). These are owned; `ParseSourceFile` is shared via `Rc` so cloning a cursor
// or span is cheap.
// ---------------------------------------------------------------------------

/// A parsed source file: its full content and an identifying URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseSourceFile {
    pub content: String,
    pub url: String,
}

impl ParseSourceFile {
    pub fn new(content: impl Into<String>, url: impl Into<String>) -> Rc<ParseSourceFile> {
        Rc::new(ParseSourceFile {
            content: content.into(),
            url: url.into(),
        })
    }
}

/// A location within a [`ParseSourceFile`]: a byte/char offset plus 0-based line and column.
#[derive(Debug, Clone)]
pub struct ParseLocation {
    pub file: Rc<ParseSourceFile>,
    /// Offset measured in `char`s (UTF code points), matching JS string indexing semantics
    /// closely enough for ASCII-heavy templates.
    pub offset: usize,
    pub line: usize,
    pub col: usize,
}

impl PartialEq for ParseLocation {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset && self.line == other.line && self.col == other.col
    }
}

impl ParseLocation {
    pub fn new(file: Rc<ParseSourceFile>, offset: usize, line: usize, col: usize) -> Self {
        ParseLocation {
            file,
            offset,
            line,
            col,
        }
    }

    /// `moveBy(delta)` — returns a new location advanced by `delta` characters, recomputing
    /// line/column from the underlying file content.
    pub fn move_by(&self, delta: isize) -> ParseLocation {
        let chars: Vec<char> = self.file.content.chars().collect();
        let len = chars.len();
        let mut offset = self.offset as isize + delta;
        if offset < 0 {
            offset = 0;
        }
        if offset as usize > len {
            offset = len as isize;
        }
        let target = offset as usize;

        // Recompute line/col by walking from the smaller of (self.offset, target).
        let mut line = self.line;
        let mut col = self.col;
        if target >= self.offset {
            let mut i = self.offset;
            while i < target {
                if chars[i] == '\n' {
                    line += 1;
                    col = 0;
                } else {
                    col += 1;
                }
                i += 1;
            }
        } else {
            // Walk backwards.
            let mut i = self.offset;
            while i > target {
                i -= 1;
                if chars[i] == '\n' {
                    line = line.saturating_sub(1);
                    // Column becomes the length of the previous line; recompute it.
                    let mut c = 0;
                    let mut j = i;
                    while j > 0 && chars[j - 1] != '\n' {
                        j -= 1;
                        c += 1;
                    }
                    col = c;
                } else {
                    col = col.saturating_sub(1);
                }
            }
        }
        ParseLocation::new(self.file.clone(), target, line, col)
    }

    /// Returns the source text on the same line, starting `max_chars` before this location.
    pub fn get_context(&self, max_chars: usize, max_lines: usize) -> Option<(String, String)> {
        let _ = max_lines;
        let chars: Vec<char> = self.file.content.chars().collect();
        if self.offset > chars.len() {
            return None;
        }
        let mut start = self.offset;
        let mut count = 0;
        while start > 0 && count < max_chars {
            start -= 1;
            count += 1;
            if chars[start] == '\n' {
                start += 1;
                break;
            }
        }
        let before: String = chars[start..self.offset].iter().collect();
        let mut end = self.offset;
        while end < chars.len() && end - self.offset < max_chars && chars[end] != '\n' {
            end += 1;
        }
        let after: String = chars[self.offset..end].iter().collect();
        Some((before, after))
    }
}

/// A span between two [`ParseLocation`]s within the same file. `full_start` is the start
/// including any leading trivia that was trimmed from `start`.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseSourceSpan {
    pub start: ParseLocation,
    pub end: ParseLocation,
    pub full_start: ParseLocation,
    pub details: Option<String>,
}

impl ParseSourceSpan {
    pub fn new(start: ParseLocation, end: ParseLocation) -> Self {
        let full_start = start.clone();
        ParseSourceSpan {
            start,
            end,
            full_start,
            details: None,
        }
    }

    pub fn with_full_start(
        start: ParseLocation,
        end: ParseLocation,
        full_start: ParseLocation,
        details: Option<String>,
    ) -> Self {
        ParseSourceSpan {
            start,
            end,
            full_start,
            details,
        }
    }

    /// The source text covered by this span (`start.offset..end.offset`).
    pub fn to_source_string(&self) -> String {
        let chars: Vec<char> = self.start.file.content.chars().collect();
        let s = self.start.offset.min(chars.len());
        let e = self.end.offset.min(chars.len());
        if s >= e {
            return String::new();
        }
        chars[s..e].iter().collect()
    }
}

/// Severity level for a [`ParseError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorLevel {
    Warning,
    Error,
}

/// A diagnostic produced during tokenization or parsing.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub span: ParseSourceSpan,
    pub msg: String,
    pub level: ParseErrorLevel,
    /// `TreeError.elementName` — the name of the element/block this error relates to, if any.
    pub element_name: Option<String>,
}

impl ParseError {
    pub fn new(span: ParseSourceSpan, msg: impl Into<String>) -> Self {
        ParseError {
            span,
            msg: msg.into(),
            level: ParseErrorLevel::Error,
            element_name: None,
        }
    }

    pub fn tree_error(
        element_name: Option<String>,
        span: ParseSourceSpan,
        msg: impl Into<String>,
    ) -> Self {
        ParseError {
            span,
            msg: msg.into(),
            level: ParseErrorLevel::Error,
            element_name,
        }
    }
}

// ---------------------------------------------------------------------------
// chars (port of `chars.ts`)
// ---------------------------------------------------------------------------

mod chars {
    pub const EOF: u32 = 0;
    pub const BSPACE: u32 = 8;
    pub const TAB: u32 = 9;
    pub const LF: u32 = 10;
    pub const VTAB: u32 = 11;
    pub const FF: u32 = 12;
    pub const CR: u32 = 13;
    pub const SPACE: u32 = 32;
    pub const BANG: u32 = 33;
    pub const DQ: u32 = 34;
    pub const HASH: u32 = 35;
    pub const DOLLAR: u32 = 36; // `$$`
    pub const AMPERSAND: u32 = 38;
    pub const SQ: u32 = 39;
    pub const LPAREN: u32 = 40;
    pub const RPAREN: u32 = 41;
    pub const STAR: u32 = 42;
    pub const COMMA: u32 = 44;
    pub const MINUS: u32 = 45;
    pub const SLASH: u32 = 47;
    pub const COLON: u32 = 58;
    pub const SEMICOLON: u32 = 59;
    pub const LT: u32 = 60;
    pub const EQ: u32 = 61;
    pub const GT: u32 = 62;

    pub const D0: u32 = 48;
    pub const D7: u32 = 55;
    pub const D9: u32 = 57;

    pub const A_UP: u32 = 65;
    pub const F_UP: u32 = 70;
    pub const X_UP: u32 = 88;
    pub const Z_UP: u32 = 90;

    pub const LBRACKET: u32 = 91;
    pub const BACKSLASH: u32 = 92;
    pub const RBRACKET: u32 = 93;
    pub const UNDERSCORE: u32 = 95;

    pub const A_LO: u32 = 97;
    pub const B_LO: u32 = 98;
    pub const F_LO: u32 = 102;
    pub const N_LO: u32 = 110;
    pub const R_LO: u32 = 114;
    pub const T_LO: u32 = 116;
    pub const U_LO: u32 = 117;
    pub const V_LO: u32 = 118;
    pub const X_LO: u32 = 120;
    pub const Z_LO: u32 = 122;

    pub const LBRACE: u32 = 123;
    pub const RBRACE: u32 = 125;
    pub const NBSP: u32 = 160;

    pub const AT: u32 = 64;
    pub const BT: u32 = 96; // backtick

    pub fn is_whitespace(code: u32) -> bool {
        (code >= TAB && code <= SPACE) || code == NBSP
    }
    pub fn is_digit(code: u32) -> bool {
        D0 <= code && code <= D9
    }
    pub fn is_ascii_letter(code: u32) -> bool {
        (code >= A_LO && code <= Z_LO) || (code >= A_UP && code <= Z_UP)
    }
    pub fn is_ascii_hex_digit(code: u32) -> bool {
        (code >= A_LO && code <= F_LO) || (code >= A_UP && code <= F_UP) || is_digit(code)
    }
    pub fn is_new_line(code: u32) -> bool {
        code == LF || code == CR
    }
    pub fn is_octal_digit(code: u32) -> bool {
        D0 <= code && code <= D7
    }
    pub fn is_quote(code: u32) -> bool {
        code == SQ || code == DQ || code == BT
    }
}

// ---------------------------------------------------------------------------
// tags (port of `tags.ts` + `html_tags.ts`)
// ---------------------------------------------------------------------------

/// How the content of a tag is tokenized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagContentType {
    RawText,
    EscapableRawText,
    ParsableData,
}

/// Definition of an HTML tag's parsing behaviour.
#[derive(Debug, Clone, PartialEq)]
pub struct TagDefinition {
    pub closed_by_children: HashMap<String, bool>,
    pub closed_by_parent: bool,
    pub implicit_namespace_prefix: Option<String>,
    pub is_void: bool,
    pub ignore_first_lf: bool,
    pub can_self_close: bool,
    pub prevent_namespace_inheritance: bool,
    content_type: TagContentType,
}

impl Default for TagDefinition {
    fn default() -> Self {
        TagDefinition {
            closed_by_children: HashMap::new(),
            closed_by_parent: false,
            implicit_namespace_prefix: None,
            is_void: false,
            ignore_first_lf: false,
            can_self_close: false,
            prevent_namespace_inheritance: false,
            content_type: TagContentType::ParsableData,
        }
    }
}

impl TagDefinition {
    pub fn is_closed_by_child(&self, name: &str) -> bool {
        self.is_void || self.closed_by_children.contains_key(&name.to_lowercase())
    }
    pub fn get_content_type(&self) -> TagContentType {
        self.content_type
    }
}

/// `splitNsName(':ns:name')` → `(Some("ns"), "name")`; otherwise `(None, name)`.
pub fn split_ns_name(element_name: &str) -> (Option<String>, String) {
    let bytes: Vec<char> = element_name.chars().collect();
    if bytes.first() != Some(&':') {
        return (None, element_name.to_string());
    }
    // indexOf(':', 1)
    let rest: String = bytes[1..].iter().collect();
    match rest.find(':') {
        None => (None, element_name.to_string()),
        Some(idx) => {
            let colon_index = idx + 1; // relative to original
            let prefix: String = bytes[1..colon_index].iter().collect();
            let name: String = bytes[colon_index + 1..].iter().collect();
            (Some(prefix), name)
        }
    }
}

pub fn get_ns_prefix(full_name: Option<&str>) -> Option<String> {
    full_name.and_then(|n| split_ns_name(n).0)
}

pub fn merge_ns_and_name(prefix: &str, local_name: &str) -> String {
    if prefix.is_empty() {
        local_name.to_string()
    } else {
        format!(":{prefix}:{local_name}")
    }
}

fn void_def() -> TagDefinition {
    TagDefinition {
        is_void: true,
        closed_by_parent: true,
        can_self_close: true,
        ..Default::default()
    }
}

fn closed_by(children: &[&str], closed_by_parent: bool) -> TagDefinition {
    let mut map = HashMap::new();
    for c in children {
        map.insert((*c).to_string(), true);
    }
    TagDefinition {
        closed_by_children: map,
        closed_by_parent,
        ..Default::default()
    }
}

/// Port of `getHtmlTagDefinition`. Builds the well-known tag table on first use.
pub fn get_html_tag_definition(tag_name: &str) -> TagDefinition {
    thread_local! {
        static DEFS: HashMap<String, TagDefinition> = build_tag_definitions();
    }
    DEFS.with(|defs| {
        defs.get(tag_name)
            .or_else(|| defs.get(&tag_name.to_lowercase()))
            .cloned()
            .unwrap_or_else(default_tag_definition)
    })
}

fn default_tag_definition() -> TagDefinition {
    TagDefinition {
        can_self_close: true,
        ..Default::default()
    }
}

fn build_tag_definitions() -> HashMap<String, TagDefinition> {
    let mut m: HashMap<String, TagDefinition> = HashMap::new();
    for v in [
        "base", "meta", "area", "embed", "link", "img", "input", "param", "hr", "br", "source",
        "track", "wbr", "col",
    ] {
        m.insert(v.to_string(), void_def());
    }
    m.insert(
        "p".to_string(),
        closed_by(
            &[
                "address", "article", "aside", "blockquote", "div", "dl", "fieldset", "footer",
                "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hgroup", "hr", "main",
                "nav", "ol", "p", "pre", "section", "table", "ul",
            ],
            true,
        ),
    );
    m.insert("thead".to_string(), closed_by(&["tbody", "tfoot"], false));
    m.insert("tbody".to_string(), closed_by(&["tbody", "tfoot"], true));
    m.insert("tfoot".to_string(), closed_by(&["tbody"], true));
    m.insert("tr".to_string(), closed_by(&["tr"], true));
    m.insert("td".to_string(), closed_by(&["td", "th"], true));
    m.insert("th".to_string(), closed_by(&["td", "th"], true));
    m.insert(
        "svg".to_string(),
        TagDefinition {
            implicit_namespace_prefix: Some("svg".to_string()),
            ..Default::default()
        },
    );
    m.insert(
        "foreignObject".to_string(),
        TagDefinition {
            implicit_namespace_prefix: Some("svg".to_string()),
            prevent_namespace_inheritance: true,
            ..Default::default()
        },
    );
    m.insert(
        "math".to_string(),
        TagDefinition {
            implicit_namespace_prefix: Some("math".to_string()),
            ..Default::default()
        },
    );
    m.insert("li".to_string(), closed_by(&["li"], true));
    m.insert("dt".to_string(), closed_by(&["dt", "dd"], false));
    m.insert("dd".to_string(), closed_by(&["dt", "dd"], true));
    m.insert("rb".to_string(), closed_by(&["rb", "rt", "rtc", "rp"], true));
    m.insert("rt".to_string(), closed_by(&["rb", "rt", "rtc", "rp"], true));
    m.insert("rtc".to_string(), closed_by(&["rb", "rtc", "rp"], true));
    m.insert("rp".to_string(), closed_by(&["rb", "rt", "rtc", "rp"], true));
    m.insert("optgroup".to_string(), closed_by(&["optgroup"], true));
    m.insert("option".to_string(), closed_by(&["option", "optgroup"], true));
    m.insert(
        "pre".to_string(),
        TagDefinition {
            ignore_first_lf: true,
            ..Default::default()
        },
    );
    m.insert(
        "listing".to_string(),
        TagDefinition {
            ignore_first_lf: true,
            ..Default::default()
        },
    );
    m.insert(
        "style".to_string(),
        TagDefinition {
            content_type: TagContentType::RawText,
            ..Default::default()
        },
    );
    m.insert(
        "script".to_string(),
        TagDefinition {
            content_type: TagContentType::RawText,
            ..Default::default()
        },
    );
    m.insert(
        "title".to_string(),
        TagDefinition {
            content_type: TagContentType::EscapableRawText,
            ..Default::default()
        },
    );
    m.insert(
        "textarea".to_string(),
        TagDefinition {
            content_type: TagContentType::EscapableRawText,
            ignore_first_lf: true,
            ..Default::default()
        },
    );
    m
}

// ---------------------------------------------------------------------------
// Named entities (minimal subset; port of `entities.ts` would be large). We include the
// common ones; unknown named entities are reported as errors, matching Angular behaviour.
// ---------------------------------------------------------------------------

fn named_entity(name: &str) -> Option<&'static str> {
    match name {
        "amp" => Some("&"),
        "lt" => Some("<"),
        "gt" => Some(">"),
        "quot" => Some("\""),
        "apos" => Some("'"),
        "nbsp" => Some("\u{a0}"),
        "copy" => Some("\u{a9}"),
        "reg" => Some("\u{ae}"),
        "deg" => Some("\u{b0}"),
        "hellip" => Some("\u{2026}"),
        "mdash" => Some("\u{2014}"),
        "ndash" => Some("\u{2013}"),
        "lsquo" => Some("\u{2018}"),
        "rsquo" => Some("\u{2019}"),
        "ldquo" => Some("\u{201c}"),
        "rdquo" => Some("\u{201d}"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Tokens (port of `tokens.ts`)
// ---------------------------------------------------------------------------

/// Lexer token kinds. Mirrors `TokenType` in `tokens.ts`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    TagOpenStart,
    TagOpenEnd,
    TagOpenEndVoid,
    TagClose,
    IncompleteTagOpen,
    Text,
    EscapableRawText,
    RawText,
    Interpolation,
    EncodedEntity,
    CommentStart,
    CommentEnd,
    CdataStart,
    CdataEnd,
    AttrName,
    AttrQuote,
    AttrValueText,
    AttrValueInterpolation,
    DocType,
    ExpansionFormStart,
    ExpansionCaseValue,
    ExpansionCaseExpStart,
    ExpansionCaseExpEnd,
    ExpansionFormEnd,
    BlockOpenStart,
    BlockOpenEnd,
    BlockClose,
    BlockParameter,
    IncompleteBlockOpen,
    LetStart,
    LetValue,
    LetEnd,
    IncompleteLet,
    ComponentOpenStart,
    ComponentOpenEnd,
    ComponentOpenEndVoid,
    ComponentClose,
    IncompleteComponentOpen,
    DirectiveName,
    DirectiveOpen,
    DirectiveClose,
    Eof,
}

/// A lexer token: a kind, its decoded `parts`, and its source span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenType,
    pub parts: Vec<String>,
    pub source_span: ParseSourceSpan,
}

impl Token {
    fn is_interpolated_text(&self) -> bool {
        matches!(
            self.kind,
            TokenType::Text | TokenType::Interpolation | TokenType::EncodedEntity
        )
    }
}

/// Result of [`tokenize`].
#[derive(Debug, Clone)]
pub struct TokenizeResult {
    pub tokens: Vec<Token>,
    pub errors: Vec<ParseError>,
    pub non_normalized_icu_expressions: Vec<Token>,
}

/// Options controlling tokenization. Mirrors `TokenizeOptions`.
#[derive(Debug, Clone)]
pub struct TokenizeOptions {
    pub tokenize_expansion_forms: bool,
    pub preserve_line_endings: bool,
    pub tokenize_blocks: bool,
    pub tokenize_let: bool,
    pub selectorless_enabled: bool,
    pub i18n_normalize_line_endings_in_icus: bool,
    pub leading_trivia_chars: Vec<char>,
}

impl Default for TokenizeOptions {
    fn default() -> Self {
        TokenizeOptions {
            tokenize_expansion_forms: false,
            preserve_line_endings: false,
            tokenize_blocks: true,
            tokenize_let: true,
            selectorless_enabled: false,
            i18n_normalize_line_endings_in_icus: false,
            leading_trivia_chars: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Character cursor (port of `PlainCharacterCursor`). Operates over a `Vec<char>` to
// match JS code-point indexing.
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Cursor {
    file: Rc<ParseSourceFile>,
    input: Rc<Vec<char>>,
    end: usize,
    peek: i64,
    offset: usize,
    line: usize,
    column: usize,
}

/// A cursor error carrying the offending cursor position (port of `CursorError`).
struct CursorError {
    msg: String,
    cursor: Cursor,
}

impl Cursor {
    fn new(file: Rc<ParseSourceFile>, input: Rc<Vec<char>>) -> Self {
        let end = input.len();
        let mut c = Cursor {
            file,
            input,
            end,
            peek: -1,
            offset: 0,
            line: 0,
            column: 0,
        };
        c.update_peek();
        c
    }

    fn char_at(&self, pos: usize) -> u32 {
        self.input[pos] as u32
    }

    fn update_peek(&mut self) {
        self.peek = if self.offset >= self.end {
            chars::EOF as i64
        } else {
            self.char_at(self.offset) as i64
        };
    }

    fn peek(&self) -> u32 {
        self.peek as u32
    }

    fn chars_left(&self) -> usize {
        self.end - self.offset
    }

    fn diff(&self, other: &Cursor) -> isize {
        self.offset as isize - other.offset as isize
    }

    fn advance(&mut self) -> Result<(), CursorError> {
        if self.offset >= self.end {
            return Err(CursorError {
                msg: "Unexpected character \"EOF\"".to_string(),
                cursor: self.clone(),
            });
        }
        let current = self.char_at(self.offset);
        if current == chars::LF {
            self.line += 1;
            self.column = 0;
        } else if !chars::is_new_line(current) {
            self.column += 1;
        }
        self.offset += 1;
        self.update_peek();
        Ok(())
    }

    fn get_chars(&self, start: &Cursor) -> String {
        self.input[start.offset..self.offset].iter().collect()
    }

    fn location(&self) -> ParseLocation {
        ParseLocation::new(self.file.clone(), self.offset, self.line, self.column)
    }

    /// Port of `getSpan(start, leadingTriviaCodePoints)`.
    fn get_span(&self, start: Option<&Cursor>, leading_trivia: &[u32]) -> ParseSourceSpan {
        let mut start_cursor = start.cloned().unwrap_or_else(|| self.clone());
        let full_start = start_cursor.clone();
        let mut moved = false;
        if !leading_trivia.is_empty() {
            while self.diff(&start_cursor) > 0 && leading_trivia.contains(&start_cursor.peek()) {
                let _ = start_cursor.advance();
                moved = true;
            }
        }
        let start_loc = start_cursor.location();
        let end_loc = self.location();
        let full_start_loc = if moved {
            full_start.location()
        } else {
            start_loc.clone()
        };
        ParseSourceSpan::with_full_start(start_loc, end_loc, full_start_loc, None)
    }
}

// ---------------------------------------------------------------------------
// Tokenizer (port of `_Tokenizer`).
// ---------------------------------------------------------------------------

const INTERPOLATION_START: &str = "{{";
const INTERPOLATION_END: &str = "}}";

const SUPPORTED_BLOCKS: &[&str] = &[
    "@if",
    "@else",
    "@for",
    "@switch",
    "@case",
    "@default",
    "@empty",
    "@defer",
    "@placeholder",
    "@loading",
    "@error",
];

type TagDefResolver = dyn Fn(&str) -> TagDefinition;

struct Tokenizer<'a> {
    cursor: Cursor,
    get_tag_definition: &'a TagDefResolver,
    tokenize_icu: bool,
    leading_trivia: Vec<u32>,
    current_token_start: Option<Cursor>,
    current_token_type: Option<TokenType>,
    expansion_case_stack: Vec<TokenType>,
    open_directive_count: i32,
    in_interpolation: bool,
    preserve_line_endings: bool,
    i18n_normalize_line_endings_in_icus: bool,
    tokenize_blocks: bool,
    tokenize_let: bool,
    selectorless_enabled: bool,
    tokens: Vec<Token>,
    errors: Vec<ParseError>,
    non_normalized_icu_expressions: Vec<Token>,
    /// Scratch slot carrying the (prefix, local name) of the element open tag from
    /// `consume_tag_open` to the raw-text content-type decision.
    pending_tag_names: Option<(String, String)>,
}

/// Internal control flow: a thrown `ParseError` (caught by `tokenize()`'s per-iteration
/// handler) or a `CursorError` (converted into a `ParseError`).
enum Thrown {
    Parse(ParseError),
    Cursor(CursorError),
}

impl From<CursorError> for Thrown {
    fn from(e: CursorError) -> Self {
        Thrown::Cursor(e)
    }
}

type TResult<T> = Result<T, Thrown>;

impl<'a> Tokenizer<'a> {
    fn new(
        file: Rc<ParseSourceFile>,
        get_tag_definition: &'a TagDefResolver,
        options: &TokenizeOptions,
    ) -> Self {
        let input: Rc<Vec<char>> = Rc::new(file.content.chars().collect());
        let leading_trivia = options
            .leading_trivia_chars
            .iter()
            .map(|c| *c as u32)
            .collect();
        Tokenizer {
            cursor: Cursor::new(file, input),
            get_tag_definition,
            tokenize_icu: options.tokenize_expansion_forms,
            leading_trivia,
            current_token_start: None,
            current_token_type: None,
            expansion_case_stack: Vec::new(),
            open_directive_count: 0,
            in_interpolation: false,
            preserve_line_endings: options.preserve_line_endings,
            i18n_normalize_line_endings_in_icus: options.i18n_normalize_line_endings_in_icus,
            tokenize_blocks: options.tokenize_blocks,
            tokenize_let: options.tokenize_let,
            selectorless_enabled: options.selectorless_enabled,
            tokens: Vec::new(),
            errors: Vec::new(),
            non_normalized_icu_expressions: Vec::new(),
            pending_tag_names: None,
        }
    }

    fn process_carriage_returns(&self, content: &str) -> String {
        if self.preserve_line_endings {
            return content.to_string();
        }
        // Replace `\r\n` or `\r` with `\n`.
        let mut out = String::with_capacity(content.len());
        let mut chs = content.chars().peekable();
        while let Some(c) = chs.next() {
            if c == '\r' {
                if chs.peek() == Some(&'\n') {
                    chs.next();
                }
                out.push('\n');
            } else {
                out.push(c);
            }
        }
        out
    }

    fn tokenize(&mut self) {
        while self.cursor.peek() != chars::EOF {
            let start = self.cursor.clone();
            if let Err(e) = self.tokenize_step(&start) {
                self.handle_error(e);
            }
        }
        self.begin_token(TokenType::Eof, None);
        let _ = self.end_token(vec![], None);
    }

    fn tokenize_step(&mut self, start: &Cursor) -> TResult<()> {
        if self.attempt_char_code(chars::LT)? {
            if self.attempt_char_code(chars::BANG)? {
                if self.attempt_char_code(chars::LBRACKET)? {
                    self.consume_cdata(start)?;
                } else if self.attempt_char_code(chars::MINUS)? {
                    self.consume_comment(start)?;
                } else {
                    self.consume_doc_type(start)?;
                }
            } else if self.attempt_char_code(chars::SLASH)? {
                self.consume_tag_close(start)?;
            } else {
                self.consume_tag_open(start)?;
            }
        } else if self.tokenize_let
            && self.cursor.peek() == chars::AT
            && !self.in_interpolation
            && self.is_let_start()
        {
            self.consume_let_declaration(start)?;
        } else if self.tokenize_blocks && self.is_block_start() {
            self.consume_block_start(start)?;
        } else if self.tokenize_blocks
            && !self.in_interpolation
            && !self.is_in_expansion_case()
            && !self.is_in_expansion_form()
            && self.attempt_char_code(chars::RBRACE)?
        {
            self.consume_block_end(start);
        } else if !(self.tokenize_icu && self.tokenize_expansion_form()?) {
            self.consume_with_interpolation(
                TokenType::Text,
                TokenType::Interpolation,
                &Tokenizer::is_text_end,
                &Tokenizer::is_tag_start,
            )?;
        }
        Ok(())
    }

    // --- block name / start / params ---

    fn get_block_name(&mut self) -> TResult<String> {
        let mut spaces_in_name_allowed = false;
        let name_cursor = self.cursor.clone();
        self.attempt_char_code_until_fn(|code| {
            if chars::is_whitespace(code) {
                return !spaces_in_name_allowed;
            }
            if is_block_name_char(code) {
                spaces_in_name_allowed = true;
                return false;
            }
            true
        })?;
        Ok(self.cursor.get_chars(&name_cursor).trim().to_string())
    }

    fn consume_block_start(&mut self, start: &Cursor) -> TResult<()> {
        self.require_char_code(chars::AT)?;
        self.begin_token(TokenType::BlockOpenStart, Some(start.clone()));
        let name = self.get_block_name()?;
        let start_idx = self.end_token(vec![name.clone()], None);

        if self.cursor.peek() == chars::LPAREN {
            self.cursor.advance()?;
            self.consume_block_parameters()?;
            self.attempt_char_code_until_fn(is_not_whitespace)?;
            if self.attempt_char_code(chars::RPAREN)? {
                self.attempt_char_code_until_fn(is_not_whitespace)?;
            } else {
                self.tokens[start_idx].kind = TokenType::IncompleteBlockOpen;
                return Ok(());
            }
        }

        if name == "default never" && self.attempt_char_code(chars::SEMICOLON)? {
            self.begin_token(TokenType::BlockOpenEnd, None);
            self.end_token(vec![], None);
            self.begin_token(TokenType::BlockClose, None);
            self.end_token(vec![], None);
            return Ok(());
        }

        if self.attempt_char_code(chars::LBRACE)? {
            self.begin_token(TokenType::BlockOpenEnd, None);
            self.end_token(vec![], None);
        } else if self.is_block_start() && (name == "case" || name == "default") {
            self.begin_token(TokenType::BlockOpenEnd, None);
            self.end_token(vec![], None);
            self.begin_token(TokenType::BlockClose, None);
            self.end_token(vec![], None);
        } else {
            self.tokens[start_idx].kind = TokenType::IncompleteBlockOpen;
        }
        Ok(())
    }

    fn consume_block_end(&mut self, start: &Cursor) {
        self.begin_token(TokenType::BlockClose, Some(start.clone()));
        self.end_token(vec![], None);
    }

    fn consume_block_parameters(&mut self) -> TResult<()> {
        self.attempt_char_code_until_fn(is_block_parameter_char)?;
        while self.cursor.peek() != chars::RPAREN && self.cursor.peek() != chars::EOF {
            self.begin_token(TokenType::BlockParameter, None);
            let start = self.cursor.clone();
            let mut in_quote: Option<u32> = None;
            let mut open_parens = 0i32;

            while (self.cursor.peek() != chars::SEMICOLON && self.cursor.peek() != chars::EOF)
                || in_quote.is_some()
            {
                let ch = self.cursor.peek();
                if ch == chars::BACKSLASH {
                    self.cursor.advance()?;
                } else if Some(ch) == in_quote {
                    in_quote = None;
                } else if in_quote.is_none() && chars::is_quote(ch) {
                    in_quote = Some(ch);
                } else if ch == chars::LPAREN && in_quote.is_none() {
                    open_parens += 1;
                } else if ch == chars::RPAREN && in_quote.is_none() {
                    if open_parens == 0 {
                        break;
                    } else if open_parens > 0 {
                        open_parens -= 1;
                    }
                }
                self.cursor.advance()?;
            }
            let text = self.cursor.get_chars(&start);
            self.end_token(vec![text], None);
            self.attempt_char_code_until_fn(is_block_parameter_char)?;
        }
        Ok(())
    }

    // --- @let ---

    fn consume_let_declaration(&mut self, start: &Cursor) -> TResult<()> {
        self.require_str("@let")?;
        self.begin_token(TokenType::LetStart, Some(start.clone()));

        if chars::is_whitespace(self.cursor.peek()) {
            self.attempt_char_code_until_fn(is_not_whitespace)?;
        } else {
            let text = self.cursor.get_chars(start);
            let idx = self.end_token(vec![text], None);
            self.tokens[idx].kind = TokenType::IncompleteLet;
            return Ok(());
        }

        let name = self.get_let_declaration_name()?;
        let start_idx = self.end_token(vec![name], None);

        self.attempt_char_code_until_fn(is_not_whitespace)?;

        if !self.attempt_char_code(chars::EQ)? {
            self.tokens[start_idx].kind = TokenType::IncompleteLet;
            return Ok(());
        }

        self.attempt_char_code_until_fn(|code| is_not_whitespace(code) && !chars::is_new_line(code))?;
        self.consume_let_declaration_value()?;

        let end_char = self.cursor.peek();
        if end_char == chars::SEMICOLON {
            self.begin_token(TokenType::LetEnd, None);
            self.cursor.advance()?;
            self.end_token(vec![], None);
        } else {
            self.tokens[start_idx].kind = TokenType::IncompleteLet;
            let span = self.cursor.get_span(Some(start), &self.leading_trivia);
            self.tokens[start_idx].source_span = span;
        }
        Ok(())
    }

    fn get_let_declaration_name(&mut self) -> TResult<String> {
        let name_cursor = self.cursor.clone();
        let mut allow_digit = false;
        self.attempt_char_code_until_fn(|code| {
            if chars::is_ascii_letter(code)
                || code == chars::DOLLAR
                || code == chars::UNDERSCORE
                || (allow_digit && chars::is_digit(code))
            {
                allow_digit = true;
                return false;
            }
            true
        })?;
        Ok(self.cursor.get_chars(&name_cursor).trim().to_string())
    }

    fn consume_let_declaration_value(&mut self) -> TResult<()> {
        let start = self.cursor.clone();
        self.begin_token(TokenType::LetValue, Some(start.clone()));
        while self.cursor.peek() != chars::EOF {
            let ch = self.cursor.peek();
            if ch == chars::SEMICOLON {
                break;
            }
            if chars::is_quote(ch) {
                self.cursor.advance()?;
                self.attempt_char_code_until_fn(|inner| {
                    // Note: backslash skipping handled below via separate advance is not
                    // possible inside a pure predicate; approximate by stopping on quote.
                    inner == ch
                })?;
            }
            self.cursor.advance()?;
        }
        let text = self.cursor.get_chars(&start);
        self.end_token(vec![text], None);
        Ok(())
    }

    // --- ICU expansion ---

    fn tokenize_expansion_form(&mut self) -> TResult<bool> {
        if self.is_expansion_form_start() {
            self.consume_expansion_form_start()?;
            return Ok(true);
        }
        if is_expansion_case_start(self.cursor.peek()) && self.is_in_expansion_form() {
            self.consume_expansion_case_start()?;
            return Ok(true);
        }
        if self.cursor.peek() == chars::RBRACE {
            if self.is_in_expansion_case() {
                self.consume_expansion_case_end()?;
                return Ok(true);
            }
            if self.is_in_expansion_form() {
                self.consume_expansion_form_end()?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn consume_expansion_form_start(&mut self) -> TResult<()> {
        self.begin_token(TokenType::ExpansionFormStart, None);
        self.require_char_code(chars::LBRACE)?;
        self.end_token(vec![], None);

        self.expansion_case_stack
            .push(TokenType::ExpansionFormStart);

        self.begin_token(TokenType::RawText, None);
        let condition = self.read_until(chars::COMMA)?;
        let normalized = self.process_carriage_returns(&condition);
        if self.i18n_normalize_line_endings_in_icus {
            self.end_token(vec![normalized], None);
        } else {
            let idx = self.end_token(vec![condition.clone()], None);
            if normalized != condition {
                self.non_normalized_icu_expressions
                    .push(self.tokens[idx].clone());
            }
        }
        self.require_char_code(chars::COMMA)?;
        self.attempt_char_code_until_fn(is_not_whitespace)?;

        self.begin_token(TokenType::RawText, None);
        let ty = self.read_until(chars::COMMA)?;
        self.end_token(vec![ty], None);
        self.require_char_code(chars::COMMA)?;
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        Ok(())
    }

    fn consume_expansion_case_start(&mut self) -> TResult<()> {
        self.begin_token(TokenType::ExpansionCaseValue, None);
        let value = self.read_until(chars::LBRACE)?.trim().to_string();
        self.end_token(vec![value], None);
        self.attempt_char_code_until_fn(is_not_whitespace)?;

        self.begin_token(TokenType::ExpansionCaseExpStart, None);
        self.require_char_code(chars::LBRACE)?;
        self.end_token(vec![], None);
        self.attempt_char_code_until_fn(is_not_whitespace)?;

        self.expansion_case_stack
            .push(TokenType::ExpansionCaseExpStart);
        Ok(())
    }

    fn consume_expansion_case_end(&mut self) -> TResult<()> {
        self.begin_token(TokenType::ExpansionCaseExpEnd, None);
        self.require_char_code(chars::RBRACE)?;
        self.end_token(vec![], None);
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        self.expansion_case_stack.pop();
        Ok(())
    }

    fn consume_expansion_form_end(&mut self) -> TResult<()> {
        self.begin_token(TokenType::ExpansionFormEnd, None);
        self.require_char_code(chars::RBRACE)?;
        self.end_token(vec![], None);
        self.expansion_case_stack.pop();
        Ok(())
    }

    // --- token bookkeeping ---

    fn begin_token(&mut self, kind: TokenType, start: Option<Cursor>) {
        self.current_token_start = Some(start.unwrap_or_else(|| self.cursor.clone()));
        self.current_token_type = Some(kind);
    }

    fn end_token(&mut self, parts: Vec<String>, end: Option<Cursor>) -> usize {
        let start = self
            .current_token_start
            .take()
            .expect("attempted to end a token when there was no start");
        let kind = self
            .current_token_type
            .take()
            .expect("attempted to end a token which has no token type");
        let end_cursor = end.unwrap_or_else(|| self.cursor.clone());
        let span = end_cursor.get_span(Some(&start), &self.leading_trivia);
        self.tokens.push(Token {
            kind,
            parts,
            source_span: span,
        });
        self.tokens.len() - 1
    }

    fn create_error(&mut self, mut msg: String, span: ParseSourceSpan) -> ParseError {
        if self.is_in_expansion_form() {
            msg += " (Do you have an unescaped \"{\" in your template? Use \"{{ '{' }}\") to escape it.)";
        }
        self.current_token_start = None;
        self.current_token_type = None;
        ParseError::new(span, msg)
    }

    fn handle_error(&mut self, e: Thrown) {
        let parse_err = match e {
            Thrown::Cursor(ce) => {
                let span = ce.cursor.get_span(None, &self.leading_trivia);
                self.create_error(ce.msg, span)
            }
            Thrown::Parse(pe) => pe,
        };
        self.errors.push(parse_err);
    }

    // --- low-level cursor helpers ---

    fn attempt_char_code(&mut self, char_code: u32) -> TResult<bool> {
        if self.cursor.peek() == char_code {
            self.cursor.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn attempt_char_code_case_insensitive(&mut self, char_code: u32) -> TResult<bool> {
        if compare_char_code_case_insensitive(self.cursor.peek(), char_code) {
            self.cursor.advance()?;
            return Ok(true);
        }
        Ok(false)
    }

    fn require_char_code(&mut self, char_code: u32) -> TResult<()> {
        let location = self.cursor.clone();
        if !self.attempt_char_code(char_code)? {
            let span = self.cursor.get_span(Some(&location), &self.leading_trivia);
            let msg = unexpected_character_error_msg(self.cursor.peek());
            let err = self.create_error(msg, span);
            return Err(Thrown::Parse(err));
        }
        Ok(())
    }

    fn attempt_str(&mut self, s: &str) -> TResult<bool> {
        let chs: Vec<char> = s.chars().collect();
        if self.cursor.chars_left() < chs.len() {
            return Ok(false);
        }
        let initial = self.cursor.clone();
        for c in &chs {
            if !self.attempt_char_code(*c as u32)? {
                self.cursor = initial;
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn attempt_str_case_insensitive(&mut self, s: &str) -> TResult<bool> {
        for c in s.chars() {
            if !self.attempt_char_code_case_insensitive(c as u32)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn require_str(&mut self, s: &str) -> TResult<()> {
        let location = self.cursor.clone();
        if !self.attempt_str(s)? {
            let span = self.cursor.get_span(Some(&location), &self.leading_trivia);
            let msg = unexpected_character_error_msg(self.cursor.peek());
            let err = self.create_error(msg, span);
            return Err(Thrown::Parse(err));
        }
        Ok(())
    }

    fn attempt_char_code_until_fn<F: FnMut(u32) -> bool>(&mut self, mut predicate: F) -> TResult<()> {
        while !predicate(self.cursor.peek()) {
            self.cursor.advance()?;
        }
        Ok(())
    }

    fn require_char_code_until_fn<F: FnMut(u32) -> bool>(
        &mut self,
        predicate: F,
        len: isize,
    ) -> TResult<()> {
        let start = self.cursor.clone();
        self.attempt_char_code_until_fn(predicate)?;
        if self.cursor.diff(&start) < len {
            let span = self.cursor.get_span(Some(&start), &self.leading_trivia);
            let msg = unexpected_character_error_msg(self.cursor.peek());
            let err = self.create_error(msg, span);
            return Err(Thrown::Parse(err));
        }
        Ok(())
    }

    fn attempt_until_char(&mut self, ch: u32) -> TResult<()> {
        while self.cursor.peek() != ch {
            self.cursor.advance()?;
        }
        Ok(())
    }

    fn read_char(&mut self) -> TResult<String> {
        let ch = char::from_u32(self.cursor.peek()).unwrap_or('\u{fffd}');
        self.cursor.advance()?;
        Ok(ch.to_string())
    }

    fn peek_str(&self, s: &str) -> bool {
        let chs: Vec<char> = s.chars().collect();
        if self.cursor.chars_left() < chs.len() {
            return false;
        }
        let mut cursor = self.cursor.clone();
        for c in &chs {
            if cursor.peek() != *c as u32 {
                return false;
            }
            if cursor.advance().is_err() {
                return false;
            }
        }
        true
    }

    fn is_block_start(&self) -> bool {
        self.cursor.peek() == chars::AT && SUPPORTED_BLOCKS.iter().any(|b| self.peek_str(b))
    }

    fn is_let_start(&self) -> bool {
        self.cursor.peek() == chars::AT && self.peek_str("@let")
    }

    // --- entities ---

    fn consume_entity(&mut self, text_token_type: TokenType) -> TResult<()> {
        self.begin_token(TokenType::EncodedEntity, None);
        let start = self.cursor.clone();
        self.cursor.advance()?;
        if self.attempt_char_code(chars::HASH)? {
            let is_hex = self.attempt_char_code(chars::X_LO)? || self.attempt_char_code(chars::X_UP)?;
            let code_start = self.cursor.clone();
            self.attempt_char_code_until_fn(is_digit_entity_end)?;
            if self.cursor.peek() != chars::SEMICOLON {
                self.cursor.advance()?;
                let span = self.cursor.get_span(None, &self.leading_trivia);
                let entity_str = self.cursor.get_chars(&start);
                let kind = if is_hex { "hexadecimal" } else { "decimal" };
                let msg = format!(
                    "Unable to parse entity \"{entity_str}\" - {kind} character reference entities must end with \";\""
                );
                let err = self.create_error(msg, span);
                return Err(Thrown::Parse(err));
            }
            let str_num = self.cursor.get_chars(&code_start);
            self.cursor.advance()?;
            let radix = if is_hex { 16 } else { 10 };
            match u32::from_str_radix(str_num.trim(), radix).ok().and_then(char::from_u32) {
                Some(c) => {
                    let encoded = self.cursor.get_chars(&start);
                    self.end_token(vec![c.to_string(), encoded], None);
                }
                None => {
                    let span = self.cursor.get_span(None, &self.leading_trivia);
                    let entity_str = self.cursor.get_chars(&start);
                    let msg = unknown_entity_error_msg(&entity_str);
                    let err = self.create_error(msg, span);
                    return Err(Thrown::Parse(err));
                }
            }
        } else {
            let name_start = self.cursor.clone();
            self.attempt_char_code_until_fn(is_named_entity_end)?;
            if self.cursor.peek() != chars::SEMICOLON {
                self.begin_token(text_token_type, Some(start));
                self.cursor = name_start;
                self.end_token(vec!["&".to_string()], None);
            } else {
                let name = self.cursor.get_chars(&name_start);
                self.cursor.advance()?;
                match named_entity(&name) {
                    Some(ch) => {
                        self.end_token(vec![ch.to_string(), format!("&{name};")], None);
                    }
                    None => {
                        let span = self.cursor.get_span(Some(&start), &self.leading_trivia);
                        let msg = unknown_entity_error_msg(&name);
                        let err = self.create_error(msg, span);
                        return Err(Thrown::Parse(err));
                    }
                }
            }
        }
        Ok(())
    }

    fn consume_raw_text<F>(&mut self, consume_entities: bool, mut end_marker: F) -> TResult<()>
    where
        F: FnMut(&mut Self) -> TResult<bool>,
    {
        self.begin_token(
            if consume_entities {
                TokenType::EscapableRawText
            } else {
                TokenType::RawText
            },
            None,
        );
        let mut parts: Vec<String> = Vec::new();
        loop {
            let tag_close_start = self.cursor.clone();
            let found = end_marker(self)?;
            self.cursor = tag_close_start;
            if found {
                break;
            }
            if consume_entities && self.cursor.peek() == chars::AMPERSAND {
                let joined = self.process_carriage_returns(&parts.concat());
                self.end_token(vec![joined], None);
                parts.clear();
                self.consume_entity(TokenType::EscapableRawText)?;
                self.begin_token(TokenType::EscapableRawText, None);
            } else {
                parts.push(self.read_char()?);
            }
        }
        let joined = self.process_carriage_returns(&parts.concat());
        self.end_token(vec![joined], None);
        Ok(())
    }

    fn consume_comment(&mut self, start: &Cursor) -> TResult<()> {
        self.begin_token(TokenType::CommentStart, Some(start.clone()));
        self.require_char_code(chars::MINUS)?;
        self.end_token(vec![], None);
        self.consume_raw_text(false, |s| s.attempt_str("-->"))?;
        self.begin_token(TokenType::CommentEnd, None);
        self.require_str("-->")?;
        self.end_token(vec![], None);
        Ok(())
    }

    fn consume_cdata(&mut self, start: &Cursor) -> TResult<()> {
        self.begin_token(TokenType::CdataStart, Some(start.clone()));
        self.require_str("CDATA[")?;
        self.end_token(vec![], None);
        self.consume_raw_text(false, |s| s.attempt_str("]]>"))?;
        self.begin_token(TokenType::CdataEnd, None);
        self.require_str("]]>")?;
        self.end_token(vec![], None);
        Ok(())
    }

    fn consume_doc_type(&mut self, start: &Cursor) -> TResult<()> {
        self.begin_token(TokenType::DocType, Some(start.clone()));
        let content_start = self.cursor.clone();
        self.attempt_until_char(chars::GT)?;
        let content = self.cursor.get_chars(&content_start);
        self.cursor.advance()?;
        self.end_token(vec![content], None);
        Ok(())
    }

    fn consume_prefix_and_name<F: FnMut(u32) -> bool>(
        &mut self,
        end_predicate: F,
    ) -> TResult<Vec<String>> {
        let name_or_prefix_start = self.cursor.clone();
        let mut prefix = String::new();
        while self.cursor.peek() != chars::COLON && !is_prefix_end(self.cursor.peek()) {
            self.cursor.advance()?;
        }
        let name_start;
        if self.cursor.peek() == chars::COLON {
            prefix = self.cursor.get_chars(&name_or_prefix_start);
            self.cursor.advance()?;
            name_start = self.cursor.clone();
        } else {
            name_start = name_or_prefix_start;
        }
        let len = if prefix.is_empty() { 0 } else { 1 };
        self.require_char_code_until_fn(end_predicate, len)?;
        let name = self.cursor.get_chars(&name_start);
        Ok(vec![prefix, name])
    }

    fn consume_single_line_comment(&mut self) -> TResult<()> {
        self.attempt_char_code_until_fn(|code| chars::is_new_line(code) || code == chars::EOF)?;
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        Ok(())
    }

    fn consume_multi_line_comment(&mut self) -> TResult<()> {
        // Scan to `*/` or EOF.
        loop {
            let code = self.cursor.peek();
            if code == chars::EOF {
                break;
            }
            if code == chars::STAR {
                let mut next = self.cursor.clone();
                let _ = next.advance();
                if next.peek() == chars::SLASH {
                    break;
                }
            }
            self.cursor.advance()?;
        }
        if self.attempt_str("*/")? {
            self.attempt_char_code_until_fn(is_not_whitespace)?;
        }
        Ok(())
    }

    fn consume_tag_open(&mut self, start: &Cursor) -> TResult<()> {
        // `try_consume_tag_open_inner` performs the fallible part and, on success, records
        // the open-token index, component-ness, and (prefix, tag_name) via `pending_tag_names`.
        match self.try_consume_tag_open_inner(start) {
            Ok((open_idx, is_component)) => {
                let (_prefix, tag_name) = self.pending_tag_names.take().unwrap_or_default();
                if is_component {
                    // Selectorless raw-text content resolution is not modelled here; nothing more.
                    return Ok(());
                }
                let content_token_type = (self.get_tag_definition)(&tag_name).get_content_type();
                match content_token_type {
                    TagContentType::RawText => {
                        let closing = closing_tag_name_for(&self.tokens[open_idx]);
                        self.consume_raw_text_with_tag_close(
                            open_idx,
                            &closing,
                            false,
                            TokenType::TagClose,
                        )?;
                    }
                    TagContentType::EscapableRawText => {
                        let closing = closing_tag_name_for(&self.tokens[open_idx]);
                        self.consume_raw_text_with_tag_close(
                            open_idx,
                            &closing,
                            true,
                            TokenType::TagClose,
                        )?;
                    }
                    TagContentType::ParsableData => {}
                }
                Ok(())
            }
            Err((Thrown::Parse(_), open_token_idx, is_component)) => {
                // We errored before the opening tag closed: mark it incomplete, or emit a
                // bare `<` text token when there was no open token at all.
                if let Some(idx) = open_token_idx {
                    self.tokens[idx].kind = if is_component {
                        TokenType::IncompleteComponentOpen
                    } else {
                        TokenType::IncompleteTagOpen
                    };
                } else {
                    self.begin_token(TokenType::Text, Some(start.clone()));
                    self.end_token(vec!["<".to_string()], None);
                }
                Ok(())
            }
            Err((other, _, _)) => Err(other),
        }
    }

    /// Returns `(open_token_index, is_component)` on success. On error, returns the thrown
    /// value alongside the open-token index (if any) and whether it was a component, so the
    /// caller can mark it incomplete.
    #[allow(clippy::type_complexity)]
    fn try_consume_tag_open_inner(
        &mut self,
        start: &Cursor,
    ) -> Result<(usize, bool), (Thrown, Option<usize>, bool)> {
        if self.selectorless_enabled && is_selectorless_name_start(self.cursor.peek()) {
            let idx = self
                .consume_component_open_start(start)
                .map_err(|e| (e, None, true))?;
            self.attempt_char_code_until_fn(is_not_whitespace)
                .map_err(|e| (e, Some(idx), true))?;
            self.consume_tag_attributes(true)
                .map_err(|e| (e, Some(idx), true))?;
            self.consume_component_open_end()
                .map_err(|e| (e, Some(idx), true))?;
            self.pending_tag_names = Some((String::new(), String::new()));
            Ok((idx, true))
        } else {
            if !chars::is_ascii_letter(self.cursor.peek()) {
                let span = self.cursor.get_span(Some(start), &self.leading_trivia);
                let msg = unexpected_character_error_msg(self.cursor.peek());
                let err = self.create_error(msg, span);
                return Err((Thrown::Parse(err), None, false));
            }
            let idx = self
                .consume_tag_open_start(start)
                .map_err(|e| (e, None, false))?;
            let parts = self.tokens[idx].parts.clone();
            let local_prefix = parts[0].clone();
            let local_tag = parts[1].clone();
            self.attempt_char_code_until_fn(is_not_whitespace)
                .map_err(|e| (e, Some(idx), false))?;
            self.consume_tag_attributes(false)
                .map_err(|e| (e, Some(idx), false))?;
            self.consume_tag_open_end()
                .map_err(|e| (e, Some(idx), false))?;
            self.pending_tag_names = Some((local_prefix, local_tag));
            Ok((idx, false))
        }
    }

    fn consume_raw_text_with_tag_close(
        &mut self,
        open_idx: usize,
        tag_name: &str,
        consume_entities: bool,
        close_kind: TokenType,
    ) -> TResult<()> {
        let tag_name_owned = tag_name.to_string();
        self.consume_raw_text(consume_entities, move |s| {
            if !s.attempt_char_code(chars::LT)? {
                return Ok(false);
            }
            if !s.attempt_char_code(chars::SLASH)? {
                return Ok(false);
            }
            s.attempt_char_code_until_fn(is_not_whitespace)?;
            if !s.attempt_str_case_insensitive(&tag_name_owned)? {
                return Ok(false);
            }
            s.attempt_char_code_until_fn(is_not_whitespace)?;
            s.attempt_char_code(chars::GT)
        })?;
        self.begin_token(close_kind, None);
        self.require_char_code_until_fn(|code| code == chars::GT, 3)?;
        self.cursor.advance()?;
        let parts = self.tokens[open_idx].parts.clone();
        self.end_token(parts, None);
        Ok(())
    }

    fn consume_tag_open_start(&mut self, start: &Cursor) -> TResult<usize> {
        self.begin_token(TokenType::TagOpenStart, Some(start.clone()));
        let parts = self.consume_prefix_and_name(is_name_end)?;
        Ok(self.end_token(parts, None))
    }

    fn consume_component_open_start(&mut self, start: &Cursor) -> TResult<usize> {
        self.begin_token(TokenType::ComponentOpenStart, Some(start.clone()));
        let parts = self.consume_component_name()?;
        Ok(self.end_token(parts, None))
    }

    fn consume_component_name(&mut self) -> TResult<Vec<String>> {
        let name_start = self.cursor.clone();
        while is_selectorless_name_char(self.cursor.peek()) {
            self.cursor.advance()?;
        }
        let name = self.cursor.get_chars(&name_start);
        let mut prefix = String::new();
        let mut tag_name = String::new();
        if self.cursor.peek() == chars::COLON {
            self.cursor.advance()?;
            let parts = self.consume_prefix_and_name(is_name_end)?;
            prefix = parts[0].clone();
            tag_name = parts[1].clone();
        }
        Ok(vec![name, prefix, tag_name])
    }

    fn consume_tag_attributes(&mut self, selectorless: bool) -> TResult<()> {
        loop {
            if self.attempt_str("//")? {
                self.consume_single_line_comment()?;
                continue;
            }
            if self.attempt_str("/*")? {
                self.consume_multi_line_comment()?;
                continue;
            }
            if is_attribute_terminator(self.cursor.peek()) {
                break;
            }
            if selectorless && self.cursor.peek() == chars::AT {
                let start = self.cursor.clone();
                let mut name_start = start.clone();
                let _ = name_start.advance();
                if is_selectorless_name_start(name_start.peek()) {
                    self.consume_directive(&start, &name_start)?;
                    continue;
                }
                // Fall through to attribute parsing for `@` not starting a directive.
                self.consume_attribute()?;
            } else {
                self.consume_attribute()?;
            }
        }
        Ok(())
    }

    fn consume_attribute(&mut self) -> TResult<()> {
        self.consume_attribute_name()?;
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        if self.attempt_char_code(chars::EQ)? {
            self.attempt_char_code_until_fn(is_not_whitespace)?;
            self.consume_attribute_value()?;
        }
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        Ok(())
    }

    fn consume_attribute_name(&mut self) -> TResult<()> {
        let attr_name_start = self.cursor.peek();
        if attr_name_start == chars::SQ || attr_name_start == chars::DQ {
            let span = self.cursor.get_span(None, &self.leading_trivia);
            let msg = unexpected_character_error_msg(attr_name_start);
            let err = self.create_error(msg, span);
            return Err(Thrown::Parse(err));
        }
        self.begin_token(TokenType::AttrName, None);

        let prefix_and_name = if self.open_directive_count > 0 {
            let mut open_parens = 0i32;
            let directive_count = self.open_directive_count;
            self.consume_prefix_and_name(|code| {
                if directive_count > 0 {
                    if code == chars::LPAREN {
                        open_parens += 1;
                    } else if code == chars::RPAREN {
                        if open_parens == 0 {
                            return true;
                        }
                        open_parens -= 1;
                    }
                }
                is_name_end(code)
            })?
        } else if attr_name_start == chars::LBRACKET {
            let mut open_brackets = 0i32;
            self.consume_prefix_and_name(|code| {
                if code == chars::LBRACKET {
                    open_brackets += 1;
                } else if code == chars::RBRACKET {
                    open_brackets -= 1;
                }
                if open_brackets <= 0 {
                    is_name_end(code)
                } else {
                    chars::is_new_line(code)
                }
            })?
        } else {
            self.consume_prefix_and_name(is_name_end)?
        };
        self.end_token(prefix_and_name, None);
        Ok(())
    }

    fn consume_attribute_value(&mut self) -> TResult<()> {
        if self.cursor.peek() == chars::SQ || self.cursor.peek() == chars::DQ {
            let quote_char = self.cursor.peek();
            self.consume_quote(quote_char)?;
            let end_pred = move |s: &Tokenizer| s.cursor.peek() == quote_char;
            self.consume_with_interpolation(
                TokenType::AttrValueText,
                TokenType::AttrValueInterpolation,
                &end_pred,
                &end_pred,
            )?;
            self.consume_quote(quote_char)?;
        } else {
            let end_pred = |s: &Tokenizer| is_name_end(s.cursor.peek());
            self.consume_with_interpolation(
                TokenType::AttrValueText,
                TokenType::AttrValueInterpolation,
                &end_pred,
                &end_pred,
            )?;
        }
        Ok(())
    }

    fn consume_quote(&mut self, quote_char: u32) -> TResult<()> {
        self.begin_token(TokenType::AttrQuote, None);
        self.require_char_code(quote_char)?;
        let ch = char::from_u32(quote_char).unwrap_or('"');
        self.end_token(vec![ch.to_string()], None);
        Ok(())
    }

    fn consume_tag_open_end(&mut self) -> TResult<()> {
        let token_type = if self.attempt_char_code(chars::SLASH)? {
            TokenType::TagOpenEndVoid
        } else {
            TokenType::TagOpenEnd
        };
        self.begin_token(token_type, None);
        self.require_char_code(chars::GT)?;
        self.end_token(vec![], None);
        Ok(())
    }

    fn consume_component_open_end(&mut self) -> TResult<()> {
        let token_type = if self.attempt_char_code(chars::SLASH)? {
            TokenType::ComponentOpenEndVoid
        } else {
            TokenType::ComponentOpenEnd
        };
        self.begin_token(token_type, None);
        self.require_char_code(chars::GT)?;
        self.end_token(vec![], None);
        Ok(())
    }

    fn consume_tag_close(&mut self, start: &Cursor) -> TResult<()> {
        if self.selectorless_enabled {
            let mut clone = start.clone();
            while clone.peek() != chars::GT && !is_selectorless_name_start(clone.peek()) {
                if clone.advance().is_err() {
                    break;
                }
            }
            if is_selectorless_name_start(clone.peek()) {
                self.begin_token(TokenType::ComponentClose, Some(start.clone()));
                let parts = self.consume_component_name()?;
                self.attempt_char_code_until_fn(is_not_whitespace)?;
                self.require_char_code(chars::GT)?;
                self.end_token(parts, None);
                return Ok(());
            }
        }
        self.begin_token(TokenType::TagClose, Some(start.clone()));
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        let prefix_and_name = self.consume_prefix_and_name(is_name_end)?;
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        self.require_char_code(chars::GT)?;
        self.end_token(prefix_and_name, None);
        Ok(())
    }

    fn consume_with_interpolation<EP, EI>(
        &mut self,
        text_token_type: TokenType,
        interpolation_token_type: TokenType,
        end_predicate: &EP,
        end_interpolation: &EI,
    ) -> TResult<()>
    where
        EP: Fn(&Tokenizer<'a>) -> bool,
        EI: Fn(&Tokenizer<'a>) -> bool,
    {
        self.begin_token(text_token_type, None);
        let mut parts: Vec<String> = Vec::new();
        while !end_predicate(self) {
            let current = self.cursor.clone();
            if self.attempt_str(INTERPOLATION_START)? {
                let joined = self.process_carriage_returns(&parts.concat());
                self.end_token(vec![joined], Some(current.clone()));
                parts.clear();
                self.consume_interpolation(interpolation_token_type, &current, Some(end_interpolation))?;
                self.begin_token(text_token_type, None);
            } else if self.cursor.peek() == chars::AMPERSAND {
                let joined = self.process_carriage_returns(&parts.concat());
                self.end_token(vec![joined], None);
                parts.clear();
                self.consume_entity(text_token_type)?;
                self.begin_token(text_token_type, None);
            } else {
                parts.push(self.read_char()?);
            }
        }
        self.in_interpolation = false;
        let joined = self.process_carriage_returns(&parts.concat());
        self.end_token(vec![joined], None);
        Ok(())
    }

    fn consume_interpolation<EI>(
        &mut self,
        interpolation_token_type: TokenType,
        interpolation_start: &Cursor,
        premature_end: Option<&EI>,
    ) -> TResult<()>
    where
        EI: Fn(&Tokenizer<'a>) -> bool,
    {
        let mut parts: Vec<String> = vec![INTERPOLATION_START.to_string()];
        self.begin_token(interpolation_token_type, Some(interpolation_start.clone()));

        let expression_start = self.cursor.clone();
        let mut in_quote: Option<u32> = None;
        let mut in_comment = false;

        while self.cursor.peek() != chars::EOF
            && (premature_end.is_none() || !(premature_end.unwrap())(self))
        {
            let current = self.cursor.clone();
            if self.is_tag_start() {
                self.cursor = current.clone();
                let processed = self.get_processed_chars(&expression_start, &current);
                parts.push(processed);
                self.end_token(parts, None);
                return Ok(());
            }

            if in_quote.is_none() {
                if self.attempt_str(INTERPOLATION_END)? {
                    let processed = self.get_processed_chars(&expression_start, &current);
                    parts.push(processed);
                    parts.push(INTERPOLATION_END.to_string());
                    self.end_token(parts, None);
                    return Ok(());
                } else if self.attempt_str("//")? {
                    in_comment = true;
                }
            }

            let ch = self.cursor.peek();
            self.cursor.advance()?;
            if ch == chars::BACKSLASH {
                self.cursor.advance()?;
            } else if Some(ch) == in_quote {
                in_quote = None;
            } else if !in_comment && in_quote.is_none() && chars::is_quote(ch) {
                in_quote = Some(ch);
            }
        }

        let processed = self.get_processed_chars(&expression_start, &self.cursor.clone());
        parts.push(processed);
        self.end_token(parts, None);
        Ok(())
    }

    fn consume_directive(&mut self, start: &Cursor, name_start: &Cursor) -> TResult<()> {
        self.require_char_code(chars::AT)?;
        // Skip over the @ (already at it after require advanced past it? require_char_code
        // consumes the @, so advance once more per Angular which advances `_cursor` again).
        self.cursor.advance()?;
        while is_selectorless_name_char(self.cursor.peek()) {
            self.cursor.advance()?;
        }
        self.begin_token(TokenType::DirectiveName, Some(start.clone()));
        let name = self.cursor.get_chars(name_start);
        self.end_token(vec![name], None);
        self.attempt_char_code_until_fn(is_not_whitespace)?;

        if self.cursor.peek() != chars::LPAREN {
            return Ok(());
        }

        self.open_directive_count += 1;
        self.begin_token(TokenType::DirectiveOpen, None);
        self.cursor.advance()?;
        self.end_token(vec![], None);
        self.attempt_char_code_until_fn(is_not_whitespace)?;

        while !is_attribute_terminator(self.cursor.peek()) && self.cursor.peek() != chars::RPAREN {
            self.consume_attribute()?;
        }

        self.attempt_char_code_until_fn(is_not_whitespace)?;
        self.open_directive_count -= 1;

        if self.cursor.peek() != chars::RPAREN {
            if self.cursor.peek() == chars::GT || self.cursor.peek() == chars::SLASH {
                return Ok(());
            }
            let span = self.cursor.get_span(Some(start), &self.leading_trivia);
            let msg = unexpected_character_error_msg(self.cursor.peek());
            let err = self.create_error(msg, span);
            return Err(Thrown::Parse(err));
        }

        self.begin_token(TokenType::DirectiveClose, None);
        self.cursor.advance()?;
        self.end_token(vec![], None);
        self.attempt_char_code_until_fn(is_not_whitespace)?;
        Ok(())
    }

    fn get_processed_chars(&self, start: &Cursor, end: &Cursor) -> String {
        self.process_carriage_returns(&end.get_chars(start))
    }

    fn is_text_end(&self) -> bool {
        if self.is_tag_start() || self.cursor.peek() == chars::EOF {
            return true;
        }
        if self.tokenize_icu && !self.in_interpolation {
            if self.is_expansion_form_start() {
                return true;
            }
            if self.cursor.peek() == chars::RBRACE && self.is_in_expansion_case() {
                return true;
            }
        }
        if self.tokenize_blocks
            && !self.in_interpolation
            && !self.is_in_expansion()
            && (self.is_block_start() || self.is_let_start() || self.cursor.peek() == chars::RBRACE)
        {
            return true;
        }
        false
    }

    fn is_tag_start(&self) -> bool {
        if self.cursor.peek() == chars::LT {
            let mut tmp = self.cursor.clone();
            let _ = tmp.advance();
            let code = tmp.peek();
            if (chars::A_LO <= code && code <= chars::Z_LO)
                || (chars::A_UP <= code && code <= chars::Z_UP)
                || code == chars::SLASH
                || code == chars::BANG
            {
                return true;
            }
        }
        false
    }

    fn read_until(&mut self, ch: u32) -> TResult<String> {
        let start = self.cursor.clone();
        self.attempt_until_char(ch)?;
        Ok(self.cursor.get_chars(&start))
    }

    fn is_in_expansion(&self) -> bool {
        self.is_in_expansion_case() || self.is_in_expansion_form()
    }

    fn is_in_expansion_case(&self) -> bool {
        self.expansion_case_stack.last() == Some(&TokenType::ExpansionCaseExpStart)
    }

    fn is_in_expansion_form(&self) -> bool {
        self.expansion_case_stack.last() == Some(&TokenType::ExpansionFormStart)
    }

    fn is_expansion_form_start(&self) -> bool {
        if self.cursor.peek() != chars::LBRACE {
            return false;
        }
        // Not an interpolation start `{{`.
        !self.peek_str(INTERPOLATION_START)
    }
}

// The `Tokenizer` needs a place to carry the captured prefix/tag name between
// `consume_tag_open` and the raw-text decision. Stored as an extra field.
impl<'a> Tokenizer<'a> {
    // (field declared via the struct below)
}

// Add the pending field by re-declaring through a newtype is not possible; instead the
// field is part of the struct. (Declared inline above would require editing the struct, so
// we keep it as an associated mutable slot using a thread-unsafe approach is avoided — the
// field is added to the struct definition.)

fn closing_tag_name_for(token: &Token) -> String {
    // For TAG_OPEN_START parts = [prefix, name]; closing tag is the local name.
    token.parts.get(1).cloned().unwrap_or_default()
}

// --- free predicate helpers (port of lexer.ts module functions) ---

fn is_not_whitespace(code: u32) -> bool {
    !chars::is_whitespace(code) || code == chars::EOF
}

fn is_name_end(code: u32) -> bool {
    chars::is_whitespace(code)
        || code == chars::GT
        || code == chars::LT
        || code == chars::SLASH
        || code == chars::SQ
        || code == chars::DQ
        || code == chars::EQ
        || code == chars::EOF
}

fn is_prefix_end(code: u32) -> bool {
    (code < chars::A_LO || chars::Z_LO < code)
        && (code < chars::A_UP || chars::Z_UP < code)
        && (code < chars::D0 || code > chars::D9)
}

fn is_digit_entity_end(code: u32) -> bool {
    code == chars::SEMICOLON || code == chars::EOF || !chars::is_ascii_hex_digit(code)
}

fn is_named_entity_end(code: u32) -> bool {
    code == chars::SEMICOLON
        || code == chars::EOF
        || !(chars::is_ascii_letter(code) || chars::is_digit(code))
}

fn is_expansion_case_start(peek: u32) -> bool {
    peek != chars::RBRACE
}

fn compare_char_code_case_insensitive(c1: u32, c2: u32) -> bool {
    to_upper_case_char_code(c1) == to_upper_case_char_code(c2)
}

fn to_upper_case_char_code(code: u32) -> u32 {
    if code >= chars::A_LO && code <= chars::Z_LO {
        code - chars::A_LO + chars::A_UP
    } else {
        code
    }
}

fn is_block_name_char(code: u32) -> bool {
    chars::is_ascii_letter(code) || chars::is_digit(code) || code == chars::UNDERSCORE
}

fn is_block_parameter_char(code: u32) -> bool {
    code != chars::SEMICOLON && is_not_whitespace(code)
}

fn is_selectorless_name_start(code: u32) -> bool {
    code == chars::UNDERSCORE || (code >= chars::A_UP && code <= chars::Z_UP)
}

fn is_selectorless_name_char(code: u32) -> bool {
    chars::is_ascii_letter(code) || chars::is_digit(code) || code == chars::UNDERSCORE
}

fn is_attribute_terminator(code: u32) -> bool {
    code == chars::SLASH || code == chars::GT || code == chars::LT || code == chars::EOF
}

fn unexpected_character_error_msg(char_code: u32) -> String {
    let c = if char_code == chars::EOF {
        "EOF".to_string()
    } else {
        char::from_u32(char_code)
            .map(|c| c.to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    format!("Unexpected character \"{c}\"")
}

fn unknown_entity_error_msg(entity_src: &str) -> String {
    format!(
        "Unknown entity \"{entity_src}\" - use the \"&#<decimal>;\" or  \"&#x<hex>;\" syntax"
    )
}

fn merge_text_tokens(src: Vec<Token>) -> Vec<Token> {
    let mut dst: Vec<Token> = Vec::with_capacity(src.len());
    for token in src {
        let merge = matches!(
            dst.last(),
            Some(last)
                if (last.kind == TokenType::Text && token.kind == TokenType::Text)
                    || (last.kind == TokenType::AttrValueText
                        && token.kind == TokenType::AttrValueText)
        );
        if merge {
            let last = dst.last_mut().unwrap();
            if last.parts.is_empty() {
                last.parts.push(String::new());
            }
            let add = token.parts.first().cloned().unwrap_or_default();
            last.parts[0].push_str(&add);
            last.source_span.end = token.source_span.end;
        } else {
            dst.push(token);
        }
    }
    dst
}

/// Port of `tokenize()`.
pub fn tokenize(
    source: &str,
    url: &str,
    get_tag_definition: &TagDefResolver,
    options: &TokenizeOptions,
) -> TokenizeResult {
    let file = ParseSourceFile::new(source, url);
    let mut tokenizer = Tokenizer::new(file, get_tag_definition, options);
    tokenizer.tokenize();
    TokenizeResult {
        tokens: merge_text_tokens(tokenizer.tokens),
        errors: tokenizer.errors,
        non_normalized_icu_expressions: tokenizer.non_normalized_icu_expressions,
    }
}

// ---------------------------------------------------------------------------
// HTML AST (port of `ast.ts`).
// ---------------------------------------------------------------------------

/// A text node, possibly containing interpolation/entity tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    pub value: String,
    pub source_span: ParseSourceSpan,
    pub tokens: Vec<Token>,
}

/// An ICU expansion form (`{ value, type, case {...} ... }`).
#[derive(Debug, Clone, PartialEq)]
pub struct Expansion {
    pub switch_value: String,
    pub r#type: String,
    pub cases: Vec<ExpansionCase>,
    pub source_span: ParseSourceSpan,
    pub switch_value_source_span: ParseSourceSpan,
}

/// A single case within an [`Expansion`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExpansionCase {
    pub value: String,
    pub expression: Vec<Node>,
    pub source_span: ParseSourceSpan,
    pub value_source_span: ParseSourceSpan,
    pub exp_source_span: ParseSourceSpan,
}

/// An element/component/directive attribute.
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: String,
    pub value: String,
    pub source_span: ParseSourceSpan,
    pub key_span: Option<ParseSourceSpan>,
    pub value_span: Option<ParseSourceSpan>,
    pub value_tokens: Option<Vec<Token>>,
}

/// An HTML element.
#[derive(Debug, Clone, PartialEq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<Attribute>,
    pub directives: Vec<Directive>,
    pub children: Vec<Node>,
    pub is_self_closing: bool,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
    pub is_void: bool,
}

/// An HTML comment.
#[derive(Debug, Clone, PartialEq)]
pub struct Comment {
    pub value: Option<String>,
    pub source_span: ParseSourceSpan,
}

/// A control-flow block (`@if`, `@for`, `@switch`, `@defer`, `@case`, etc.).
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub name: String,
    pub parameters: Vec<BlockParameter>,
    pub children: Vec<Node>,
    pub source_span: ParseSourceSpan,
    pub name_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
}

/// A parameter of a [`Block`] (raw expression text).
#[derive(Debug, Clone, PartialEq)]
pub struct BlockParameter {
    pub expression: String,
    pub source_span: ParseSourceSpan,
}

/// A selectorless component node (`<MyComponent .../>`).
#[derive(Debug, Clone, PartialEq)]
pub struct Component {
    pub component_name: String,
    pub tag_name: Option<String>,
    pub full_name: String,
    pub attrs: Vec<Attribute>,
    pub directives: Vec<Directive>,
    pub children: Vec<Node>,
    pub is_self_closing: bool,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
}

/// A selectorless directive applied to an element/component (`@Dir(...)`).
#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    pub name: String,
    pub attrs: Vec<Attribute>,
    pub source_span: ParseSourceSpan,
    pub start_source_span: ParseSourceSpan,
    pub end_source_span: Option<ParseSourceSpan>,
}

/// A `@let name = value;` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct LetDeclaration {
    pub name: String,
    pub value: String,
    pub source_span: ParseSourceSpan,
    pub name_span: ParseSourceSpan,
    pub value_span: ParseSourceSpan,
}

/// An HTML AST node (port of the `html.Node` union).
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Element(Box<Element>),
    Text(Box<Text>),
    Comment(Box<Comment>),
    Attribute(Box<Attribute>),
    Expansion(Box<Expansion>),
    ExpansionCase(Box<ExpansionCase>),
    Block(Box<Block>),
    BlockParameter(Box<BlockParameter>),
    Component(Box<Component>),
    Directive(Box<Directive>),
    LetDeclaration(Box<LetDeclaration>),
}

impl Node {
    pub fn source_span(&self) -> &ParseSourceSpan {
        match self {
            Node::Element(n) => &n.source_span,
            Node::Text(n) => &n.source_span,
            Node::Comment(n) => &n.source_span,
            Node::Attribute(n) => &n.source_span,
            Node::Expansion(n) => &n.source_span,
            Node::ExpansionCase(n) => &n.source_span,
            Node::Block(n) => &n.source_span,
            Node::BlockParameter(n) => &n.source_span,
            Node::Component(n) => &n.source_span,
            Node::Directive(n) => &n.source_span,
            Node::LetDeclaration(n) => &n.source_span,
        }
    }
}

/// Visitor over the HTML AST (port of `html.Visitor`). Each method has a default no-op so
/// implementors only override what they need.
pub trait Visitor {
    fn visit_element(&mut self, _element: &Element) {}
    fn visit_attribute(&mut self, _attribute: &Attribute) {}
    fn visit_text(&mut self, _text: &Text) {}
    fn visit_comment(&mut self, _comment: &Comment) {}
    fn visit_expansion(&mut self, _expansion: &Expansion) {}
    fn visit_expansion_case(&mut self, _expansion_case: &ExpansionCase) {}
    fn visit_block(&mut self, _block: &Block) {}
    fn visit_block_parameter(&mut self, _parameter: &BlockParameter) {}
    fn visit_let_declaration(&mut self, _decl: &LetDeclaration) {}
    fn visit_component(&mut self, _component: &Component) {}
    fn visit_directive(&mut self, _directive: &Directive) {}
}

/// Port of `visitAll`.
pub fn visit_all<V: Visitor>(visitor: &mut V, nodes: &[Node]) {
    for node in nodes {
        match node {
            Node::Element(n) => visitor.visit_element(n),
            Node::Attribute(n) => visitor.visit_attribute(n),
            Node::Text(n) => visitor.visit_text(n),
            Node::Comment(n) => visitor.visit_comment(n),
            Node::Expansion(n) => visitor.visit_expansion(n),
            Node::ExpansionCase(n) => visitor.visit_expansion_case(n),
            Node::Block(n) => visitor.visit_block(n),
            Node::BlockParameter(n) => visitor.visit_block_parameter(n),
            Node::Component(n) => visitor.visit_component(n),
            Node::Directive(n) => visitor.visit_directive(n),
            Node::LetDeclaration(n) => visitor.visit_let_declaration(n),
        }
    }
}

// ---------------------------------------------------------------------------
// Parser / TreeBuilder (port of `parser.ts`).
// ---------------------------------------------------------------------------

/// Result of [`Parser::parse`] / [`parse`].
#[derive(Debug, Clone)]
pub struct ParseTreeResult {
    pub root_nodes: Vec<Node>,
    pub errors: Vec<ParseError>,
}

/// A container node identity on the open-element stack.
#[derive(Clone)]
enum ContainerKind {
    Element,
    Block,
    Component,
}

/// HTML parser, parameterized by a tag-definition resolver. Port of `Parser`.
pub struct Parser {
    get_tag_definition: Box<TagDefResolver>,
}

impl Parser {
    pub fn new(get_tag_definition: Box<TagDefResolver>) -> Self {
        Parser { get_tag_definition }
    }

    pub fn parse(&self, source: &str, url: &str, options: &TokenizeOptions) -> ParseTreeResult {
        let tokenize_result = tokenize(source, url, self.get_tag_definition.as_ref(), options);
        let mut builder =
            TreeBuilder::new(tokenize_result.tokens, self.get_tag_definition.as_ref());
        builder.build();
        let mut errors = tokenize_result.errors;
        errors.extend(builder.errors);
        ParseTreeResult {
            root_nodes: builder.root_nodes,
            errors,
        }
    }
}

/// `HtmlParser` — a [`Parser`] preconfigured with the HTML tag definitions.
pub struct HtmlParser;

impl HtmlParser {
    pub fn parse(source: &str, url: &str, options: &TokenizeOptions) -> ParseTreeResult {
        let parser = Parser::new(Box::new(get_html_tag_definition));
        parser.parse(source, url, options)
    }
}

/// A node held on the open-container stack along with enough info to reattach children and
/// close it. We store an index path into the (growing) tree; to keep things simple and
/// owned, the tree builder buffers children on the stack and commits them on close.
struct OpenContainer {
    kind: ContainerKind,
    /// The element/block/component being built; its `children` are filled while open.
    element: Option<Element>,
    block: Option<Block>,
    component: Option<Component>,
}

impl OpenContainer {
    fn name(&self) -> Option<String> {
        match self.kind {
            ContainerKind::Element => self.element.as_ref().map(|e| e.name.clone()),
            ContainerKind::Block => self.block.as_ref().map(|b| b.name.clone()),
            ContainerKind::Component => self.component.as_ref().map(|c| c.full_name.clone()),
        }
    }

    fn push_child(&mut self, node: Node) {
        match self.kind {
            ContainerKind::Element => self.element.as_mut().unwrap().children.push(node),
            ContainerKind::Block => self.block.as_mut().unwrap().children.push(node),
            ContainerKind::Component => self.component.as_mut().unwrap().children.push(node),
        }
    }

    fn into_node(self) -> Node {
        match self.kind {
            ContainerKind::Element => Node::Element(Box::new(self.element.unwrap())),
            ContainerKind::Block => Node::Block(Box::new(self.block.unwrap())),
            ContainerKind::Component => Node::Component(Box::new(self.component.unwrap())),
        }
    }

    fn tag_name_for_def(&self) -> Option<String> {
        match self.kind {
            ContainerKind::Element => self.element.as_ref().map(|e| e.name.clone()),
            ContainerKind::Component => self
                .component
                .as_ref()
                .and_then(|c| c.tag_name.clone()),
            ContainerKind::Block => None,
        }
    }
}

struct TreeBuilder<'a> {
    tokens: Vec<Token>,
    index: isize,
    peek: Token,
    container_stack: Vec<OpenContainer>,
    root_nodes: Vec<Node>,
    errors: Vec<ParseError>,
    get_tag_definition: &'a TagDefResolver,
}

impl<'a> TreeBuilder<'a> {
    fn new(tokens: Vec<Token>, get_tag_definition: &'a TagDefResolver) -> Self {
        // There is always an EOF token at the end; clone it as the initial peek placeholder.
        let initial = tokens
            .last()
            .cloned()
            .expect("token stream must end with EOF");
        let mut tb = TreeBuilder {
            tokens,
            index: -1,
            peek: initial,
            container_stack: Vec::new(),
            root_nodes: Vec::new(),
            errors: Vec::new(),
            get_tag_definition,
        };
        tb.advance();
        tb
    }

    fn build(&mut self) {
        while self.peek.kind != TokenType::Eof {
            match self.peek.kind {
                TokenType::TagOpenStart | TokenType::IncompleteTagOpen => {
                    let t = self.advance();
                    self.consume_element_start_tag(t);
                }
                TokenType::TagClose => {
                    let t = self.advance();
                    self.consume_element_end_tag(t);
                }
                TokenType::CdataStart => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_cdata(t);
                }
                TokenType::CommentStart => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_comment(t);
                }
                TokenType::Text | TokenType::RawText | TokenType::EscapableRawText => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_text(t);
                }
                TokenType::ExpansionFormStart => {
                    let t = self.advance();
                    self.consume_expansion(t);
                }
                TokenType::BlockOpenStart => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_block_open(t);
                }
                TokenType::BlockClose => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_block_close(t);
                }
                TokenType::IncompleteBlockOpen => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_incomplete_block(t);
                }
                TokenType::LetStart => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_let(t);
                }
                TokenType::IncompleteLet => {
                    self.close_void_element();
                    let t = self.advance();
                    self.consume_incomplete_let(t);
                }
                TokenType::ComponentOpenStart | TokenType::IncompleteComponentOpen => {
                    let t = self.advance();
                    self.consume_component_start_tag(t);
                }
                TokenType::ComponentClose => {
                    let t = self.advance();
                    self.consume_component_end_tag(t);
                }
                _ => {
                    self.advance();
                }
            }
        }

        // Unclosed blocks are an error (unlike HTML elements which are closed by EOF).
        // We must drain the stack, committing nodes into their parents.
        while let Some(container) = self.container_stack.pop() {
            if matches!(container.kind, ContainerKind::Block) {
                let block = container.block.as_ref().unwrap();
                self.errors.push(ParseError::tree_error(
                    Some(block.name.clone()),
                    block.source_span.clone(),
                    format!("Unclosed block \"{}\"", block.name),
                ));
            }
            let node = container.into_node();
            self.commit_node(node);
        }
    }

    fn advance(&mut self) -> Token {
        let prev = self.peek.clone();
        if self.index < self.tokens.len() as isize - 1 {
            self.index += 1;
        }
        self.peek = self.tokens[self.index as usize].clone();
        prev
    }

    fn advance_if(&mut self, kind: TokenType) -> Option<Token> {
        if self.peek.kind == kind {
            Some(self.advance())
        } else {
            None
        }
    }

    fn consume_cdata(&mut self, _start: Token) {
        let text = self.advance();
        self.consume_text(text);
        self.advance_if(TokenType::CdataEnd);
    }

    fn consume_comment(&mut self, token: Token) {
        let text = self.advance_if(TokenType::RawText);
        let end_token = self.advance_if(TokenType::CommentEnd);
        let value = text
            .as_ref()
            .map(|t| t.parts.first().cloned().unwrap_or_default().trim().to_string());
        let source_span = match &end_token {
            None => token.source_span.clone(),
            Some(end) => ParseSourceSpan::with_full_start(
                token.source_span.start.clone(),
                end.source_span.end.clone(),
                token.source_span.full_start.clone(),
                None,
            ),
        };
        self.add_to_parent(Node::Comment(Box::new(Comment { value, source_span })));
    }

    fn consume_expansion(&mut self, token: Token) {
        let switch_value = self.advance();
        let ty = self.advance();
        let mut cases: Vec<ExpansionCase> = Vec::new();

        while self.peek.kind == TokenType::ExpansionCaseValue {
            match self.parse_expansion_case() {
                Some(c) => cases.push(c),
                None => return,
            }
        }

        if self.peek.kind != TokenType::ExpansionFormEnd {
            self.errors.push(ParseError::tree_error(
                None,
                self.peek.source_span.clone(),
                "Invalid ICU message. Missing '}'.",
            ));
            return;
        }
        let source_span = ParseSourceSpan::with_full_start(
            token.source_span.start.clone(),
            self.peek.source_span.end.clone(),
            token.source_span.full_start.clone(),
            None,
        );
        self.add_to_parent(Node::Expansion(Box::new(Expansion {
            switch_value: switch_value.parts.first().cloned().unwrap_or_default(),
            r#type: ty.parts.first().cloned().unwrap_or_default(),
            cases,
            source_span,
            switch_value_source_span: switch_value.source_span.clone(),
        })));
        self.advance();
    }

    fn parse_expansion_case(&mut self) -> Option<ExpansionCase> {
        let value = self.advance();
        if self.peek.kind != TokenType::ExpansionCaseExpStart {
            self.errors.push(ParseError::tree_error(
                None,
                self.peek.source_span.clone(),
                "Invalid ICU message. Missing '{'.",
            ));
            return None;
        }
        let start = self.advance();
        let exp = self.collect_expansion_exp_tokens(&start)?;
        let end = self.advance();

        let mut exp_with_eof = exp;
        exp_with_eof.push(Token {
            kind: TokenType::Eof,
            parts: vec![],
            source_span: end.source_span.clone(),
        });

        let mut sub_builder = TreeBuilder::new(exp_with_eof, self.get_tag_definition);
        sub_builder.build();
        if !sub_builder.errors.is_empty() {
            self.errors.extend(sub_builder.errors);
            return None;
        }

        let source_span = ParseSourceSpan::with_full_start(
            value.source_span.start.clone(),
            end.source_span.end.clone(),
            value.source_span.full_start.clone(),
            None,
        );
        let exp_source_span = ParseSourceSpan::with_full_start(
            start.source_span.start.clone(),
            end.source_span.end.clone(),
            start.source_span.full_start.clone(),
            None,
        );
        Some(ExpansionCase {
            value: value.parts.first().cloned().unwrap_or_default(),
            expression: sub_builder.root_nodes,
            source_span,
            value_source_span: value.source_span.clone(),
            exp_source_span,
        })
    }

    fn collect_expansion_exp_tokens(&mut self, start: &Token) -> Option<Vec<Token>> {
        let mut exp: Vec<Token> = Vec::new();
        let mut stack: Vec<TokenType> = vec![TokenType::ExpansionCaseExpStart];
        loop {
            if self.peek.kind == TokenType::ExpansionFormStart
                || self.peek.kind == TokenType::ExpansionCaseExpStart
            {
                stack.push(self.peek.kind);
            }

            if self.peek.kind == TokenType::ExpansionCaseExpEnd {
                if stack.last() == Some(&TokenType::ExpansionCaseExpStart) {
                    stack.pop();
                    if stack.is_empty() {
                        return Some(exp);
                    }
                } else {
                    self.errors.push(ParseError::tree_error(
                        None,
                        start.source_span.clone(),
                        "Invalid ICU message. Missing '}'.",
                    ));
                    return None;
                }
            }

            if self.peek.kind == TokenType::ExpansionFormEnd {
                if stack.last() == Some(&TokenType::ExpansionFormStart) {
                    stack.pop();
                } else {
                    self.errors.push(ParseError::tree_error(
                        None,
                        start.source_span.clone(),
                        "Invalid ICU message. Missing '}'.",
                    ));
                    return None;
                }
            }

            if self.peek.kind == TokenType::Eof {
                self.errors.push(ParseError::tree_error(
                    None,
                    start.source_span.clone(),
                    "Invalid ICU message. Missing '}'.",
                ));
                return None;
            }

            let t = self.advance();
            exp.push(t);
        }
    }

    fn consume_text(&mut self, token: Token) {
        let mut tokens = vec![token.clone()];
        let start_span = token.source_span.clone();
        let mut text = token.parts.first().cloned().unwrap_or_default();

        if !text.is_empty() && text.starts_with('\n') {
            let ignore = self
                .get_container_tag_def()
                .map(|d| d.ignore_first_lf)
                .unwrap_or(false)
                && self
                    .container_stack
                    .last()
                    .map(|c| match &c.kind {
                        ContainerKind::Element => c.element.as_ref().unwrap().children.is_empty(),
                        ContainerKind::Component => {
                            c.component.as_ref().unwrap().children.is_empty()
                        }
                        ContainerKind::Block => c.block.as_ref().unwrap().children.is_empty(),
                    })
                    .unwrap_or(false);
            if ignore {
                text = text[1..].to_string();
                tokens[0] = Token {
                    kind: token.kind,
                    parts: vec![text.clone()],
                    source_span: token.source_span.clone(),
                };
            }
        }

        let mut last_span = token.source_span.clone();
        while matches!(
            self.peek.kind,
            TokenType::Interpolation | TokenType::Text | TokenType::EncodedEntity
        ) {
            let t = self.advance();
            last_span = t.source_span.clone();
            match t.kind {
                TokenType::Interpolation => {
                    text += &decode_entities_in(&t.parts.concat());
                }
                TokenType::EncodedEntity => {
                    text += &t.parts.first().cloned().unwrap_or_default();
                }
                _ => {
                    text += &t.parts.concat();
                }
            }
            tokens.push(t);
        }

        if !text.is_empty() {
            let span = ParseSourceSpan::with_full_start(
                start_span.start.clone(),
                last_span.end.clone(),
                start_span.full_start.clone(),
                start_span.details.clone(),
            );
            self.add_to_parent(Node::Text(Box::new(Text {
                value: text,
                source_span: span,
                tokens,
            })));
        }
    }

    fn close_void_element(&mut self) {
        if let Some(def) = self.get_container_tag_def() {
            if def.is_void {
                self.pop_container_commit();
            }
        }
    }

    fn consume_element_start_tag(&mut self, start_tag_token: Token) {
        let mut attrs: Vec<Attribute> = Vec::new();
        let mut directives: Vec<Directive> = Vec::new();
        self.consume_attributes_and_directives(&mut attrs, &mut directives);

        let parent_name = self.closest_element_like_parent_name();
        let full_name = self.get_element_full_name(&start_tag_token, parent_name.as_deref());
        let tag_def = (self.get_tag_definition)(&full_name);

        let mut self_closing = false;
        if self.peek.kind == TokenType::TagOpenEndVoid {
            self.advance();
            self_closing = true;
            if !(tag_def.can_self_close
                || get_ns_prefix(Some(&full_name)).is_some()
                || tag_def.is_void)
            {
                self.errors.push(ParseError::tree_error(
                    Some(full_name.clone()),
                    start_tag_token.source_span.clone(),
                    format!(
                        "Only void, custom and foreign elements can be self closed \"{}\"",
                        start_tag_token.parts.get(1).cloned().unwrap_or_default()
                    ),
                ));
            }
        } else if self.peek.kind == TokenType::TagOpenEnd {
            self.advance();
            self_closing = false;
        }

        let end = self.peek.source_span.full_start.clone();
        let span = ParseSourceSpan::with_full_start(
            start_tag_token.source_span.start.clone(),
            end.clone(),
            start_tag_token.source_span.full_start.clone(),
            None,
        );
        let start_span = span.clone();

        let el = Element {
            name: full_name.clone(),
            attrs,
            directives,
            children: Vec::new(),
            is_self_closing: self_closing,
            source_span: span.clone(),
            start_source_span: start_span,
            end_source_span: None,
            is_void: tag_def.is_void,
        };

        let is_closed_by_child = self
            .get_container_tag_def()
            .map(|d| d.is_closed_by_child(&el.name))
            .unwrap_or(false);

        self.push_container(
            OpenContainer {
                kind: ContainerKind::Element,
                element: Some(el),
                block: None,
                component: None,
            },
            is_closed_by_child,
        );

        if self_closing {
            self.pop_container(Some(&full_name), ContainerKind::Element, Some(span));
        } else if start_tag_token.kind == TokenType::IncompleteTagOpen {
            self.pop_container(Some(&full_name), ContainerKind::Element, None);
            self.errors.push(ParseError::tree_error(
                Some(full_name.clone()),
                span,
                format!("Opening tag \"{full_name}\" not terminated."),
            ));
        }
    }

    fn consume_component_start_tag(&mut self, start_token: Token) {
        let component_name = start_token.parts.first().cloned().unwrap_or_default();
        let mut attrs: Vec<Attribute> = Vec::new();
        let mut directives: Vec<Directive> = Vec::new();
        self.consume_attributes_and_directives(&mut attrs, &mut directives);

        let closest = self.closest_element_like_parent_name();
        let tag_name = self.get_component_tag_name(&start_token, closest.as_deref());
        let full_name = self.get_component_full_name(&start_token, closest.as_deref());
        let self_closing = self.peek.kind == TokenType::ComponentOpenEndVoid;
        self.advance();

        let end = self.peek.source_span.full_start.clone();
        let span = ParseSourceSpan::with_full_start(
            start_token.source_span.start.clone(),
            end.clone(),
            start_token.source_span.full_start.clone(),
            None,
        );
        let start_span = span.clone();

        let node = Component {
            component_name,
            tag_name: tag_name.clone(),
            full_name: full_name.clone(),
            attrs,
            directives,
            children: Vec::new(),
            is_self_closing: self_closing,
            source_span: span.clone(),
            start_source_span: start_span,
            end_source_span: None,
        };

        let is_closed_by_child = tag_name
            .as_ref()
            .and_then(|tn| self.get_container_tag_def().map(|d| d.is_closed_by_child(tn)))
            .unwrap_or(false);

        self.push_container(
            OpenContainer {
                kind: ContainerKind::Component,
                element: None,
                block: None,
                component: Some(node),
            },
            is_closed_by_child,
        );

        if self_closing {
            self.pop_container(Some(&full_name), ContainerKind::Component, Some(span));
        } else if start_token.kind == TokenType::IncompleteComponentOpen {
            self.pop_container(Some(&full_name), ContainerKind::Component, None);
            self.errors.push(ParseError::tree_error(
                Some(full_name.clone()),
                span,
                format!("Opening tag \"{full_name}\" not terminated."),
            ));
        }
    }

    fn consume_attributes_and_directives(
        &mut self,
        attrs: &mut Vec<Attribute>,
        directives: &mut Vec<Directive>,
    ) {
        while self.peek.kind == TokenType::AttrName || self.peek.kind == TokenType::DirectiveName {
            if self.peek.kind == TokenType::DirectiveName {
                let name_token = self.peek.clone();
                directives.push(self.consume_directive(name_token));
            } else {
                let t = self.advance();
                attrs.push(self.consume_attr(t));
            }
        }
    }

    fn consume_component_end_tag(&mut self, end_token: Token) {
        let full_name =
            self.get_component_full_name(&end_token, self.closest_element_like_parent_name().as_deref());
        if !self.pop_container(
            Some(&full_name),
            ContainerKind::Component,
            Some(end_token.source_span.clone()),
        ) {
            let suffix =
                ". It may happen when the tag has already been closed by another tag.".to_string();
            let err_msg = format!("Unexpected closing tag \"{full_name}\"{suffix}");
            self.errors.push(ParseError::tree_error(
                Some(full_name),
                end_token.source_span.clone(),
                err_msg,
            ));
        }
    }

    fn consume_attr(&mut self, attr_name: Token) -> Attribute {
        let full_name = merge_ns_and_name(
            attr_name.parts.first().map(|s| s.as_str()).unwrap_or(""),
            attr_name.parts.get(1).map(|s| s.as_str()).unwrap_or(""),
        );
        let mut attr_end = attr_name.source_span.end.clone();

        if self.peek.kind == TokenType::AttrQuote {
            self.advance();
        }

        let mut value = String::new();
        let mut value_tokens: Vec<Token> = Vec::new();
        let mut value_start_span: Option<ParseSourceSpan> = None;
        let mut value_end: Option<ParseLocation> = None;

        if self.peek.kind == TokenType::AttrValueText {
            value_start_span = Some(self.peek.source_span.clone());
            value_end = Some(self.peek.source_span.end.clone());
            while matches!(
                self.peek.kind,
                TokenType::AttrValueText
                    | TokenType::AttrValueInterpolation
                    | TokenType::EncodedEntity
            ) {
                let value_token = self.advance();
                match value_token.kind {
                    TokenType::AttrValueInterpolation => {
                        value += &decode_entities_in(&value_token.parts.concat());
                    }
                    TokenType::EncodedEntity => {
                        value += &value_token.parts.first().cloned().unwrap_or_default();
                    }
                    _ => {
                        value += &value_token.parts.concat();
                    }
                }
                value_end = Some(value_token.source_span.end.clone());
                attr_end = value_token.source_span.end.clone();
                value_tokens.push(value_token);
            }
        }

        if self.peek.kind == TokenType::AttrQuote {
            let quote_token = self.advance();
            attr_end = quote_token.source_span.end.clone();
        }

        let value_span = match (value_start_span, value_end) {
            (Some(vss), Some(ve)) => Some(ParseSourceSpan::with_full_start(
                vss.start.clone(),
                ve,
                vss.full_start.clone(),
                None,
            )),
            _ => None,
        };

        Attribute {
            name: full_name,
            value,
            source_span: ParseSourceSpan::with_full_start(
                attr_name.source_span.start.clone(),
                attr_end,
                attr_name.source_span.full_start.clone(),
                None,
            ),
            key_span: Some(attr_name.source_span.clone()),
            value_span,
            value_tokens: if value_tokens.is_empty() {
                None
            } else {
                Some(value_tokens)
            },
        }
    }

    fn consume_directive(&mut self, name_token: Token) -> Directive {
        let mut attributes: Vec<Attribute> = Vec::new();
        let mut start_source_span_end = name_token.source_span.end.clone();
        let mut end_source_span: Option<ParseSourceSpan> = None;
        self.advance();

        if self.peek.kind == TokenType::DirectiveOpen {
            start_source_span_end = self.peek.source_span.end.clone();
            self.advance();
            while self.peek.kind == TokenType::AttrName {
                let t = self.advance();
                attributes.push(self.consume_attr(t));
            }
            if self.peek.kind == TokenType::DirectiveClose {
                end_source_span = Some(self.peek.source_span.clone());
                self.advance();
            } else {
                self.errors.push(ParseError::tree_error(
                    None,
                    name_token.source_span.clone(),
                    "Unterminated directive definition",
                ));
            }
        }

        let start_source_span = ParseSourceSpan::with_full_start(
            name_token.source_span.start.clone(),
            start_source_span_end,
            name_token.source_span.full_start.clone(),
            None,
        );
        let end = match &end_source_span {
            None => name_token.source_span.end.clone(),
            Some(e) => e.end.clone(),
        };
        let source_span = ParseSourceSpan::with_full_start(
            start_source_span.start.clone(),
            end,
            start_source_span.full_start.clone(),
            None,
        );

        Directive {
            name: name_token.parts.first().cloned().unwrap_or_default(),
            attrs: attributes,
            source_span,
            start_source_span,
            end_source_span,
        }
    }

    fn consume_block_open(&mut self, token: Token) {
        let mut parameters: Vec<BlockParameter> = Vec::new();
        while self.peek.kind == TokenType::BlockParameter {
            let param_token = self.advance();
            parameters.push(BlockParameter {
                expression: param_token.parts.first().cloned().unwrap_or_default(),
                source_span: param_token.source_span.clone(),
            });
        }
        if self.peek.kind == TokenType::BlockOpenEnd {
            self.advance();
        }
        let end = self.peek.source_span.full_start.clone();
        let span = ParseSourceSpan::with_full_start(
            token.source_span.start.clone(),
            end.clone(),
            token.source_span.full_start.clone(),
            None,
        );
        let start_span = span.clone();
        let block = Block {
            name: token.parts.first().cloned().unwrap_or_default(),
            parameters,
            children: Vec::new(),
            source_span: span,
            name_span: token.source_span.clone(),
            start_source_span: start_span,
            end_source_span: None,
        };
        self.push_container(
            OpenContainer {
                kind: ContainerKind::Block,
                element: None,
                block: Some(block),
                component: None,
            },
            false,
        );
    }

    fn consume_block_close(&mut self, token: Token) {
        let initial_len = self.container_stack.len();
        let top_name = self.container_stack.last().and_then(|c| c.name());
        if !self.pop_container(None, ContainerKind::Block, Some(token.source_span.clone())) {
            if self.container_stack.len() < initial_len {
                let node_name = top_name.unwrap_or_default();
                self.errors.push(ParseError::tree_error(
                    None,
                    token.source_span.clone(),
                    format!(
                        "Unexpected closing block. The block may have been closed earlier. \
                         Did you forget to close the <{node_name}> element? \
                         If you meant to write the `}}` character, you should use the \"&#125;\" \
                         HTML entity instead."
                    ),
                ));
                return;
            }
            self.errors.push(ParseError::tree_error(
                None,
                token.source_span.clone(),
                "Unexpected closing block. The block may have been closed earlier. \
                 If you meant to write the `}` character, you should use the \"&#125;\" \
                 HTML entity instead.",
            ));
        }
    }

    fn consume_incomplete_block(&mut self, token: Token) {
        let mut parameters: Vec<BlockParameter> = Vec::new();
        while self.peek.kind == TokenType::BlockParameter {
            let param_token = self.advance();
            parameters.push(BlockParameter {
                expression: param_token.parts.first().cloned().unwrap_or_default(),
                source_span: param_token.source_span.clone(),
            });
        }
        let end = self.peek.source_span.full_start.clone();
        let span = ParseSourceSpan::with_full_start(
            token.source_span.start.clone(),
            end.clone(),
            token.source_span.full_start.clone(),
            None,
        );
        let start_span = span.clone();
        let name = token.parts.first().cloned().unwrap_or_default();
        let block = Block {
            name: name.clone(),
            parameters,
            children: Vec::new(),
            source_span: span.clone(),
            name_span: token.source_span.clone(),
            start_source_span: start_span,
            end_source_span: None,
        };
        self.push_container(
            OpenContainer {
                kind: ContainerKind::Block,
                element: None,
                block: Some(block),
                component: None,
            },
            false,
        );
        self.pop_container(None, ContainerKind::Block, None);
        self.errors.push(ParseError::tree_error(
            Some(name.clone()),
            span,
            format!(
                "Incomplete block \"{name}\". If you meant to write the @ character, \
                 you should use the \"&#64;\" HTML entity instead."
            ),
        ));
    }

    fn consume_let(&mut self, start_token: Token) {
        let name = start_token.parts.first().cloned().unwrap_or_default();

        if self.peek.kind != TokenType::LetValue {
            self.errors.push(ParseError::tree_error(
                Some(name.clone()),
                start_token.source_span.clone(),
                format!("Invalid @let declaration \"{name}\". Declaration must have a value."),
            ));
            return;
        }
        let value_token = self.advance();

        if self.peek.kind != TokenType::LetEnd {
            self.errors.push(ParseError::tree_error(
                Some(name.clone()),
                start_token.source_span.clone(),
                format!(
                    "Unterminated @let declaration \"{name}\". Declaration must be terminated with a semicolon."
                ),
            ));
            return;
        }
        let end_token = self.advance();

        let end = end_token.source_span.end.clone();
        let span = ParseSourceSpan::with_full_start(
            start_token.source_span.start.clone(),
            end,
            start_token.source_span.full_start.clone(),
            None,
        );

        let span_text = start_token.source_span.to_source_string();
        let start_offset = span_text.rfind(&name).map(|i| i as isize).unwrap_or(0);
        let name_start = start_token.source_span.start.move_by(start_offset);
        let name_span = ParseSourceSpan::new(name_start, start_token.source_span.end.clone());

        self.add_to_parent(Node::LetDeclaration(Box::new(LetDeclaration {
            name,
            value: value_token.parts.first().cloned().unwrap_or_default(),
            source_span: span,
            name_span,
            value_span: value_token.source_span.clone(),
        })));
    }

    fn consume_incomplete_let(&mut self, token: Token) {
        let name = token.parts.first().cloned().unwrap_or_default();
        let name_string = if name.is_empty() {
            String::new()
        } else {
            format!(" \"{name}\"")
        };

        if !name.is_empty() {
            let span_text = token.source_span.to_source_string();
            let start_offset = span_text.rfind(&name).map(|i| i as isize).unwrap_or(0);
            let name_start = token.source_span.start.move_by(start_offset);
            let name_span = ParseSourceSpan::new(name_start, token.source_span.end.clone());
            let value_span = ParseSourceSpan::new(
                token.source_span.start.clone(),
                token.source_span.start.move_by(0),
            );
            self.add_to_parent(Node::LetDeclaration(Box::new(LetDeclaration {
                name: name.clone(),
                value: String::new(),
                source_span: token.source_span.clone(),
                name_span,
                value_span,
            })));
        }

        self.errors.push(ParseError::tree_error(
            Some(name),
            token.source_span.clone(),
            format!(
                "Incomplete @let declaration{name_string}. \
                 @let declarations must be written as `@let <name> = <value>;`"
            ),
        ));
    }

    fn consume_element_end_tag(&mut self, end_tag_token: Token) {
        let full_name =
            self.get_element_full_name(&end_tag_token, self.closest_element_like_parent_name().as_deref());

        if (self.get_tag_definition)(&full_name).is_void {
            self.errors.push(ParseError::tree_error(
                Some(full_name.clone()),
                end_tag_token.source_span.clone(),
                format!(
                    "Void elements do not have end tags \"{}\"",
                    end_tag_token.parts.get(1).cloned().unwrap_or_default()
                ),
            ));
        } else if !self.pop_container(
            Some(&full_name),
            ContainerKind::Element,
            Some(end_tag_token.source_span.clone()),
        ) {
            let err_msg = format!(
                "Unexpected closing tag \"{full_name}\". It may happen when the tag has already been \
                 closed by another tag. For more info see \
                 https://www.w3.org/TR/html5/syntax.html#closing-elements-that-have-implied-end-tags"
            );
            self.errors.push(ParseError::tree_error(
                Some(full_name),
                end_tag_token.source_span.clone(),
                err_msg,
            ));
        }
    }

    // --- container stack management ---

    fn push_container(&mut self, node: OpenContainer, is_closed_by_child: bool) {
        if is_closed_by_child {
            // The current top is implicitly closed by this child: commit it to its parent.
            self.pop_container_commit();
        }
        self.container_stack.push(node);
    }

    /// Commit the top container into its parent (or root) without span bookkeeping. Used for
    /// implicit closes (closedByChild) and EOF draining.
    fn pop_container_commit(&mut self) {
        if let Some(container) = self.container_stack.pop() {
            let node = container.into_node();
            self.commit_node(node);
        }
    }

    /// Port of `_popContainer`. Closes the nearest matching container, committing it (and any
    /// implicitly-closed descendants) into their parents. Returns whether the close was
    /// "expected" (no unexpected close tags encountered on the way).
    fn pop_container(
        &mut self,
        expected_name: Option<&str>,
        expected_kind: ContainerKind,
        end_source_span: Option<ParseSourceSpan>,
    ) -> bool {
        let mut unexpected = false;
        let mut found_index: Option<usize> = None;

        for stack_index in (0..self.container_stack.len()).rev() {
            let container = &self.container_stack[stack_index];
            let node_name = container.name();
            let kind_matches = same_kind(&container.kind, &expected_kind);
            let name_matches = expected_name.is_none() || node_name.as_deref() == expected_name;

            if name_matches && kind_matches {
                found_index = Some(stack_index);
                break;
            }

            // Blocks and non-closedByParent elements being skipped count as unexpected.
            let is_block = matches!(container.kind, ContainerKind::Block);
            let closed_by_parent = container
                .tag_name_for_def()
                .map(|n| (self.get_tag_definition)(&n).closed_by_parent)
                .unwrap_or(false);
            if is_block || !closed_by_parent {
                unexpected = true;
            }
        }

        match found_index {
            Some(idx) => {
                // Pop everything above and including idx, committing each into its parent.
                while self.container_stack.len() > idx {
                    let is_target = self.container_stack.len() - 1 == idx;
                    let mut container = self.container_stack.pop().unwrap();
                    if is_target {
                        apply_end_span(&mut container, end_source_span.clone());
                    }
                    let node = container.into_node();
                    self.commit_node(node);
                }
                !unexpected
            }
            None => false,
        }
    }

    fn commit_node(&mut self, node: Node) {
        if let Some(parent) = self.container_stack.last_mut() {
            parent.push_child(node);
        } else {
            self.root_nodes.push(node);
        }
    }

    fn add_to_parent(&mut self, node: Node) {
        if let Some(parent) = self.container_stack.last_mut() {
            parent.push_child(node);
        } else {
            self.root_nodes.push(node);
        }
    }

    fn get_container_tag_def(&self) -> Option<TagDefinition> {
        let container = self.container_stack.last()?;
        let name = container.tag_name_for_def()?;
        Some((self.get_tag_definition)(&name))
    }

    fn closest_element_like_parent_name(&self) -> Option<String> {
        for container in self.container_stack.iter().rev() {
            match container.kind {
                ContainerKind::Element => {
                    return container.element.as_ref().map(|e| e.name.clone());
                }
                ContainerKind::Component => {
                    return container.component.as_ref().and_then(|c| {
                        // For prefix inheritance we need the element-like name; use tag_name
                        // if present else full_name.
                        c.tag_name.clone().or(Some(c.full_name.clone()))
                    });
                }
                ContainerKind::Block => continue,
            }
        }
        None
    }

    fn get_element_full_name(&self, token: &Token, parent: Option<&str>) -> String {
        let prefix = self.get_prefix_for_tag(token, parent);
        merge_ns_and_name(&prefix, token.parts.get(1).map(|s| s.as_str()).unwrap_or(""))
    }

    fn get_component_full_name(&self, token: &Token, parent: Option<&str>) -> String {
        let component_name = token.parts.first().cloned().unwrap_or_default();
        let tag_name = self.get_component_tag_name(token, parent);
        match tag_name {
            None => component_name,
            Some(tn) => {
                if tn.starts_with(':') {
                    format!("{component_name}{tn}")
                } else {
                    format!("{component_name}:{tn}")
                }
            }
        }
    }

    fn get_component_tag_name(&self, token: &Token, parent: Option<&str>) -> Option<String> {
        let prefix = self.get_prefix_for_component(token, parent);
        let tag_name = token.parts.get(2).cloned().unwrap_or_default();
        if prefix.is_empty() && tag_name.is_empty() {
            None
        } else if prefix.is_empty() && !tag_name.is_empty() {
            Some(tag_name)
        } else {
            let local = if tag_name.is_empty() {
                "ng-component".to_string()
            } else {
                tag_name
            };
            Some(merge_ns_and_name(&prefix, &local))
        }
    }

    fn get_prefix_for_tag(&self, token: &Token, parent: Option<&str>) -> String {
        let prefix = token.parts.first().cloned().unwrap_or_default();
        let tag_name = token.parts.get(1).cloned().unwrap_or_default();
        self.resolve_prefix(prefix, tag_name, parent)
    }

    fn get_prefix_for_component(&self, token: &Token, parent: Option<&str>) -> String {
        let prefix = token.parts.get(1).cloned().unwrap_or_default();
        let tag_name = token.parts.get(2).cloned().unwrap_or_default();
        self.resolve_prefix(prefix, tag_name, parent)
    }

    fn resolve_prefix(&self, prefix: String, tag_name: String, parent: Option<&str>) -> String {
        let mut prefix = if prefix.is_empty() {
            (self.get_tag_definition)(&tag_name)
                .implicit_namespace_prefix
                .unwrap_or_default()
        } else {
            prefix
        };

        if prefix.is_empty() {
            if let Some(parent_name) = parent {
                let parent_tag_name = split_ns_name(parent_name).1;
                let parent_def = (self.get_tag_definition)(&parent_tag_name);
                if !parent_def.prevent_namespace_inheritance {
                    if let Some(p) = get_ns_prefix(Some(parent_name)) {
                        prefix = p;
                    }
                }
            }
        }

        prefix
    }
}

fn same_kind(a: &ContainerKind, b: &ContainerKind) -> bool {
    matches!(
        (a, b),
        (ContainerKind::Element, ContainerKind::Element)
            | (ContainerKind::Block, ContainerKind::Block)
            | (ContainerKind::Component, ContainerKind::Component)
    )
}

fn apply_end_span(container: &mut OpenContainer, end_source_span: Option<ParseSourceSpan>) {
    match container.kind {
        ContainerKind::Element => {
            let el = container.element.as_mut().unwrap();
            el.end_source_span = end_source_span.clone();
            if let Some(span) = &end_source_span {
                el.source_span.end = span.end.clone();
            }
        }
        ContainerKind::Block => {
            let b = container.block.as_mut().unwrap();
            b.end_source_span = end_source_span.clone();
            if let Some(span) = &end_source_span {
                b.source_span.end = span.end.clone();
            }
        }
        ContainerKind::Component => {
            let c = container.component.as_mut().unwrap();
            c.end_source_span = end_source_span.clone();
            if let Some(span) = &end_source_span {
                c.source_span.end = span.end.clone();
            }
        }
    }
}

/// Backward-compatibility helper: decode HTML entities (`&name;`, `&#x..;`, `&#..;`) that
/// appear in interpolation expression text. Port of `decodeEntity` applied over a string.
fn decode_entities_in(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars_vec: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars_vec.len() {
        if chars_vec[i] == '&' {
            // Find the closing `;`.
            if let Some(end_rel) = chars_vec[i + 1..].iter().position(|c| *c == ';') {
                let entity: String = chars_vec[i + 1..i + 1 + end_rel].iter().collect();
                let m: String = chars_vec[i..i + 1 + end_rel + 1].iter().collect();
                let decoded = decode_entity(&m, &entity);
                out.push_str(&decoded);
                i = i + 1 + end_rel + 1;
                continue;
            }
        }
        out.push(chars_vec[i]);
        i += 1;
    }
    out
}

fn decode_entity(m: &str, entity: &str) -> String {
    if let Some(c) = named_entity(entity) {
        return c.to_string();
    }
    let bytes: Vec<char> = entity.chars().collect();
    // `#x<hex>`
    if bytes.len() >= 2 && bytes[0] == '#' && (bytes[1] == 'x' || bytes[1] == 'X') {
        let hex: String = bytes[2..].iter().collect();
        if let Some(c) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
            return c.to_string();
        }
    }
    // `#<dec>`
    if bytes.len() >= 2 && bytes[0] == '#' && bytes[1..].iter().all(|c| c.is_ascii_digit()) {
        let dec: String = bytes[1..].iter().collect();
        if let Some(c) = dec.parse::<u32>().ok().and_then(char::from_u32) {
            return c.to_string();
        }
    }
    m.to_string()
}

/// Top-level convenience entry point. Parses `source` as an HTML/Angular template with ICU
/// expansion forms, `@`-control-flow blocks, and `@let` enabled.
pub fn parse(source: &str, url: &str) -> ParseTreeResult {
    let options = TokenizeOptions {
        tokenize_expansion_forms: true,
        ..TokenizeOptions::default()
    };
    HtmlParser::parse(source, url, &options)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn first_element(result: &ParseTreeResult) -> &Element {
        match &result.root_nodes[0] {
            Node::Element(e) => e,
            other => panic!("expected element, got {other:?}"),
        }
    }

    #[test]
    fn parses_div_with_attrs_and_text() {
        let result = parse("<div id=\"main\" class=\"a b\">hello world</div>", "test.html");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.root_nodes.len(), 1);
        let el = first_element(&result);
        assert_eq!(el.name, "div");
        assert_eq!(el.attrs.len(), 2);
        assert_eq!(el.attrs[0].name, "id");
        assert_eq!(el.attrs[0].value, "main");
        assert_eq!(el.attrs[1].name, "class");
        assert_eq!(el.attrs[1].value, "a b");
        assert!(!el.is_self_closing);
        assert!(el.end_source_span.is_some());
        assert_eq!(el.children.len(), 1);
        match &el.children[0] {
            Node::Text(t) => assert_eq!(t.value, "hello world"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn parses_interpolation_token_boundaries() {
        let result = parse("<span>a {{ value }} b</span>", "test.html");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let el = first_element(&result);
        assert_eq!(el.name, "span");
        assert_eq!(el.children.len(), 1);
        match &el.children[0] {
            Node::Text(t) => {
                // The combined text value includes the raw interpolation markers.
                assert_eq!(t.value, "a {{ value }} b");
                // Boundaries: TEXT, INTERPOLATION, TEXT.
                let kinds: Vec<TokenType> = t.tokens.iter().map(|tk| tk.kind).collect();
                assert!(kinds.contains(&TokenType::Interpolation));
                let interp = t
                    .tokens
                    .iter()
                    .find(|tk| tk.kind == TokenType::Interpolation)
                    .unwrap();
                assert_eq!(interp.parts[0], "{{");
                assert_eq!(interp.parts[1], " value ");
                assert_eq!(interp.parts[2], "}}");
            }
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn parses_if_block() {
        let result = parse("@if (cond) { <b>yes</b> }", "test.html");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.root_nodes.len(), 1);
        match &result.root_nodes[0] {
            Node::Block(b) => {
                assert_eq!(b.name, "if");
                assert_eq!(b.parameters.len(), 1);
                assert_eq!(b.parameters[0].expression, "cond");
                assert!(b.end_source_span.is_some());
                // Child should be the <b> element (whitespace text nodes also present).
                let has_b = b.children.iter().any(|c| matches!(c, Node::Element(e) if e.name == "b"));
                assert!(has_b, "block children: {:?}", b.children);
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    #[test]
    fn parses_icu_expansion() {
        let result = parse(
            "{count, plural, =1 {one} other {many}}",
            "test.html",
        );
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.root_nodes.len(), 1);
        match &result.root_nodes[0] {
            Node::Expansion(e) => {
                assert_eq!(e.switch_value, "count");
                assert_eq!(e.r#type, "plural");
                assert_eq!(e.cases.len(), 2);
                assert_eq!(e.cases[0].value, "=1");
                assert_eq!(e.cases[1].value, "other");
            }
            other => panic!("expected expansion, got {other:?}"),
        }
    }

    #[test]
    fn parses_comment() {
        let result = parse("<!-- hi -->", "test.html");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        match &result.root_nodes[0] {
            Node::Comment(c) => assert_eq!(c.value.as_deref(), Some("hi")),
            other => panic!("expected comment, got {other:?}"),
        }
    }

    #[test]
    fn parses_let_declaration() {
        let result = parse("@let x = 1 + 2;", "test.html");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        match &result.root_nodes[0] {
            Node::LetDeclaration(l) => {
                assert_eq!(l.name, "x");
                assert_eq!(l.value, "1 + 2");
            }
            other => panic!("expected let declaration, got {other:?}"),
        }
    }

    #[test]
    fn void_element_has_no_children() {
        let result = parse("<br><p>text</p>", "test.html");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.root_nodes.len(), 2);
        match &result.root_nodes[0] {
            Node::Element(e) => {
                assert_eq!(e.name, "br");
                assert!(e.children.is_empty());
            }
            other => panic!("expected br, got {other:?}"),
        }
    }
}
