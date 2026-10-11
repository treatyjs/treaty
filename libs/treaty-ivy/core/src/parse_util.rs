//! Source locations, spans and diagnostics shared by every Treaty compiler layer.
//!
//! Port of Angular's `packages/compiler/src/parse_util.ts` (`ParseLocation`,
//! `ParseSourceFile`, `ParseSourceSpan`, `ParseErrorLevel`, `ParseError`). This is the ONLY
//! definition of these types in the workspace: `ml_parser` (HTML tokens/AST), the render3 AST
//! (`template::r3_ast`), the expression AST (`expression::ast`), the i18n AST and the output IR
//! (`output_ast`) all use them.
//!
//! # Offset unit
//!
//! Every `offset` in this module is a count of **UTF-16 code units**, exactly as Angular's
//! offsets are JavaScript string indices (`String.prototype.charCodeAt` / `substring`). A
//! character outside the Basic Multilingual Plane (for example an emoji) therefore advances the
//! offset by 2, and a BMP character by 1, regardless of how many bytes the UTF-8 encoding of the
//! source uses. `line` and `col` are 0-based; `col` is also counted in UTF-16 code units.
//!
//! The expression lexer and parser (`expression::{lexer,parser}`), `AbsoluteSourceSpan` and
//! `ParseSpan` use the same unit, so an absolute offset coming from a `ParseSourceSpan` can be
//! handed to the expression parser unchanged. To interoperate with byte-based consumers (for
//! example oxc spans, which are UTF-8 byte offsets into the TypeScript source), use
//! [`ParseLocation::from_byte_offset`] and [`ParseLocation::byte_offset`], or the free functions
//! [`utf16_to_byte_offset`] and [`byte_to_utf16_offset`].
//!
//! Unlike the rest of this crate, `ParseSourceFile` holds an owned `String`, so a location can
//! answer `get_context`/`move_by` without the caller passing the text. Shared ownership is by
//! `Rc`, which keeps cloning a span cheap.

use std::fmt;
use std::rc::Rc;

/// A parsed source file: its full content and an identifying URL.
///
/// Port of `ParseSourceFile`. Shared through `Rc` by every [`ParseLocation`] that points into it.
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

    /// Length of the content in UTF-16 code units (JavaScript `string.length`).
    pub fn utf16_len(&self) -> usize {
        self.content.encode_utf16().count()
    }
}

/// A location in a [`ParseSourceFile`]: UTF-16 `offset`, 0-based `line` and `col`.
///
/// Port of `ParseLocation`.
#[derive(Debug, Clone)]
pub struct ParseLocation {
    pub file: Rc<ParseSourceFile>,
    /// UTF-16 code-unit offset into `file.content`.
    pub offset: usize,
    /// 0-based line.
    pub line: usize,
    /// 0-based column, in UTF-16 code units from the start of the line.
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

    /// Build the location of a UTF-8 byte offset into `file.content`. The byte offset must fall on
    /// a character boundary; an offset past the end is clamped to the end of the content.
    ///
    /// This is the bridge for byte-based producers (oxc spans). Line and column are computed the
    /// way Angular computes them, so the result is identical to a location produced by the HTML
    /// lexer at the same character.
    pub fn from_byte_offset(file: Rc<ParseSourceFile>, byte_offset: usize) -> ParseLocation {
        let content = &file.content;
        let byte_offset = byte_offset.min(content.len());
        let mut line = 0usize;
        let mut line_start = 0usize;
        for (i, b) in content.as_bytes()[..byte_offset].iter().enumerate() {
            if *b == b'\n' {
                line += 1;
                line_start = i + 1;
            }
        }
        let offset = byte_to_utf16_offset(content, byte_offset);
        let col =
            byte_to_utf16_offset(content, byte_offset) - byte_to_utf16_offset(content, line_start);
        ParseLocation::new(file, offset, line, col)
    }

    /// The UTF-8 byte offset of this location in `file.content` (inverse of
    /// [`ParseLocation::from_byte_offset`]).
    pub fn byte_offset(&self) -> usize {
        utf16_to_byte_offset(&self.file.content, self.offset)
    }

    /// `moveBy(delta)`: a new location `delta` UTF-16 code units away, with line and column
    /// recomputed from the content. Port of `ParseLocation.moveBy`.
    pub fn move_by(&self, delta: isize) -> ParseLocation {
        let source: Vec<u16> = self.file.content.encode_utf16().collect();
        let len = source.len();
        let mut offset = self.offset;
        let mut line = self.line as isize;
        let mut col = self.col as isize;
        let mut delta = delta;
        const LF: u16 = b'\n' as u16;
        while offset > 0 && delta < 0 {
            offset -= 1;
            delta += 1;
            let ch = source[offset];
            if ch == LF {
                line -= 1;
                // `source.substring(0, offset - 1).lastIndexOf('\n')`
                let prior_line = if offset >= 1 {
                    source[..offset - 1]
                        .iter()
                        .rposition(|c| *c == LF)
                        .map(|p| p as isize)
                        .unwrap_or(-1)
                } else {
                    -1
                };
                col = if prior_line > 0 {
                    offset as isize - prior_line
                } else {
                    offset as isize
                };
            } else {
                col -= 1;
            }
        }
        while offset < len && delta > 0 {
            let ch = source[offset];
            offset += 1;
            delta -= 1;
            if ch == LF {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        ParseLocation::new(self.file.clone(), offset, line as usize, col as usize)
    }

    /// `getContext(maxChars, maxLines)`: the source text around the location, as `before`
    /// (ending at the location) and `after` (starting at it). Returns `None` when the offset is
    /// out of range for the content. Port of `ParseLocation.getContext`.
    pub fn get_context(&self, max_chars: usize, max_lines: usize) -> Option<ParseContext> {
        let content: Vec<u16> = self.file.content.encode_utf16().collect();
        let mut start_offset = self.offset;
        if content.is_empty() {
            return None;
        }
        if start_offset > content.len() - 1 {
            start_offset = content.len() - 1;
        }
        let mut end_offset = start_offset;
        let mut ctx_chars = 0usize;
        let mut ctx_lines = 0usize;
        const LF: u16 = b'\n' as u16;

        while ctx_chars < max_chars && start_offset > 0 {
            start_offset -= 1;
            ctx_chars += 1;
            if content[start_offset] == LF {
                ctx_lines += 1;
                if ctx_lines == max_lines {
                    break;
                }
            }
        }

        ctx_chars = 0;
        ctx_lines = 0;
        while ctx_chars < max_chars && end_offset < content.len() - 1 {
            end_offset += 1;
            ctx_chars += 1;
            if content[end_offset] == LF {
                ctx_lines += 1;
                if ctx_lines == max_lines {
                    break;
                }
            }
        }

        Some(ParseContext {
            before: String::from_utf16_lossy(
                &content[start_offset..self.offset.min(content.len())],
            ),
            after: String::from_utf16_lossy(
                &content[self.offset.min(content.len())..(end_offset + 1).min(content.len())],
            ),
        })
    }
}

impl fmt::Display for ParseLocation {
    /// `url@line:col` (Angular's `ParseLocation.toString`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}:{}", self.file.url, self.line, self.col)
    }
}

/// The text on either side of a location, returned by [`ParseLocation::get_context`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseContext {
    pub before: String,
    pub after: String,
}

/// A span between two [`ParseLocation`]s in the same file.
///
/// Port of `ParseSourceSpan`. `start` is the location after skipping leading trivia, so an
/// element's span begins at its opening `<`; `full_start` is the location before trivia was
/// skipped (equal to `start` when there is none).
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

    /// Constructor with an explicit `full_start` and `details` (Angular's optional arguments).
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

    /// A span with no real source position, used where Angular has no source (for example
    /// synthetic nodes and JIT-only types). Its offsets are `0` and it refers to an empty file
    /// with an empty URL.
    pub fn synthetic() -> Self {
        let file = ParseSourceFile::new("", "");
        let loc = ParseLocation::new(file, 0, 0, 0);
        ParseSourceSpan::new(loc.clone(), loc)
    }

    /// A span over UTF-16 offsets `start..end` of an anonymous file whose content is `end` spaces.
    /// Intended for tests and for fixtures that only need offsets; real spans come from a parser.
    pub fn from_offsets(start: usize, end: usize) -> Self {
        let file = ParseSourceFile::new(" ".repeat(end), "");
        ParseSourceSpan::new(
            ParseLocation::new(file.clone(), start, 0, start),
            ParseLocation::new(file, end, 0, end),
        )
    }

    /// A span over the UTF-8 byte range `start_byte..end_byte` of `file.content`, as produced by
    /// byte-based front ends such as oxc (see [`ParseLocation::from_byte_offset`]).
    pub fn from_byte_range(file: Rc<ParseSourceFile>, start_byte: usize, end_byte: usize) -> Self {
        let start = ParseLocation::from_byte_offset(file.clone(), start_byte);
        let end = ParseLocation::from_byte_offset(file, end_byte);
        ParseSourceSpan::new(start, end)
    }

    /// The source text covered by this span (`start.offset..end.offset`, UTF-16 units).
    /// Port of `ParseSourceSpan.toString`.
    pub fn to_source_string(&self) -> String {
        let units: Vec<u16> = self.start.file.content.encode_utf16().collect();
        let s = self.start.offset.min(units.len());
        let e = self.end.offset.min(units.len());
        if s >= e {
            return String::new();
        }
        String::from_utf16_lossy(&units[s..e])
    }
}

/// Severity of a [`ParseError`]. Port of `ParseErrorLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseErrorLevel {
    Warning,
    Error,
}

impl ParseErrorLevel {
    /// The TypeScript enum member name (`ParseErrorLevel[level]`).
    pub fn name(self) -> &'static str {
        match self {
            ParseErrorLevel::Warning => "WARNING",
            ParseErrorLevel::Error => "ERROR",
        }
    }
}

/// A diagnostic with a location. Port of `ParseError`.
///
/// `relatedError` from the TypeScript class is not modelled: no Rust consumer reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    /// Location of the error.
    pub span: ParseSourceSpan,
    /// Error message.
    pub msg: String,
    /// Severity level of the error.
    pub level: ParseErrorLevel,
}

impl ParseError {
    /// `new ParseError(span, msg)` with the default `ERROR` level.
    pub fn new(span: ParseSourceSpan, msg: impl Into<String>) -> Self {
        ParseError {
            span,
            msg: msg.into(),
            level: ParseErrorLevel::Error,
        }
    }

    /// `new ParseError(span, msg, level)`.
    pub fn with_level(
        span: ParseSourceSpan,
        msg: impl Into<String>,
        level: ParseErrorLevel,
    ) -> Self {
        ParseError {
            span,
            msg: msg.into(),
            level,
        }
    }

    /// `contextualMessage()`: the message followed by the text around the error start, with the
    /// error marked, for example `Unexpected token ("<p>[ERROR ->]{{ a b }}</p>")`.
    pub fn contextual_message(&self) -> String {
        match self.span.start.get_context(100, 3) {
            Some(ctx) => format!(
                "{} (\"{}[{} ->]{}\")",
                self.msg,
                ctx.before,
                self.level.name(),
                ctx.after
            ),
            None => self.msg.clone(),
        }
    }
}

impl fmt::Display for ParseError {
    /// Angular's `ParseError.toString`: `contextualMessage(): <start location>[, details]`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let details = match &self.span.details {
            Some(d) if !d.is_empty() => format!(", {d}"),
            _ => String::new(),
        };
        write!(
            f,
            "{}: {}{}",
            self.contextual_message(),
            self.span.start,
            details
        )
    }
}

impl std::error::Error for ParseError {}

/// The length of `text` in UTF-16 code units (JavaScript `text.length`). Every offset the template
/// pipeline computes by adding or moving a length must use this, never `chars().count()` or `len()`.
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Convert a UTF-16 code-unit offset into a UTF-8 byte offset of `text`. Offsets past the end are
/// clamped; an offset that lands inside a surrogate pair maps to the byte offset of that
/// character's start.
pub fn utf16_to_byte_offset(text: &str, utf16_offset: usize) -> usize {
    let mut units = 0usize;
    for (byte_index, ch) in text.char_indices() {
        if units >= utf16_offset {
            return byte_index;
        }
        units += ch.len_utf16();
    }
    text.len()
}

/// Convert a UTF-8 byte offset of `text` into a UTF-16 code-unit offset. An offset inside a
/// multi-byte character maps to the offset of the character's start; past the end is clamped.
pub fn byte_to_utf16_offset(text: &str, byte_offset: usize) -> usize {
    let byte_offset = byte_offset.min(text.len());
    text[..byte_offset].encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(content: &str) -> Rc<ParseSourceFile> {
        ParseSourceFile::new(content, "test.html")
    }

    #[test]
    fn utf16_units_count_astral_characters_as_two() {
        let text = "a\u{1F600}b"; // 😀 is U+1F600: 4 UTF-8 bytes, 2 UTF-16 units
        assert_eq!(byte_to_utf16_offset(text, 0), 0);
        assert_eq!(byte_to_utf16_offset(text, 1), 1);
        assert_eq!(byte_to_utf16_offset(text, 5), 3);
        assert_eq!(byte_to_utf16_offset(text, 6), 4);
        assert_eq!(utf16_to_byte_offset(text, 1), 1);
        assert_eq!(utf16_to_byte_offset(text, 3), 5);
        assert_eq!(utf16_to_byte_offset(text, 4), 6);
        assert_eq!(utf16_to_byte_offset(text, 99), 6);
    }

    #[test]
    fn byte_offset_round_trips_through_location() {
        let f = file("é\n😀x");
        // "é" is 2 bytes (1 unit), "\n" 1 byte, "😀" 4 bytes (2 units), "x" 1 byte.
        let loc = ParseLocation::from_byte_offset(f.clone(), "é\n😀".len());
        assert_eq!((loc.line, loc.col, loc.offset), (1, 2, 4));
        assert_eq!(loc.byte_offset(), "é\n😀".len());
        let x = ParseLocation::from_byte_offset(f.clone(), "é\n😀x".len());
        assert_eq!((x.line, x.col, x.offset), (1, 3, 5));
    }

    #[test]
    fn move_by_recomputes_line_and_column_forward_and_back() {
        let f = file("ab\ncd");
        let start = ParseLocation::new(f.clone(), 0, 0, 0);
        let c = start.move_by(3);
        assert_eq!((c.offset, c.line, c.col), (3, 1, 0));
        let d = c.move_by(1);
        assert_eq!((d.offset, d.line, d.col), (4, 1, 1));
    }

    #[test]
    fn get_context_returns_text_around_the_location() {
        let f = file("line one\nline two");
        let loc = ParseLocation::new(f, 12, 1, 3);
        let ctx = loc.get_context(100, 3).unwrap();
        assert_eq!(ctx.before, "line one\nlin");
        assert_eq!(ctx.after, "e two");
    }

    #[test]
    fn span_source_string_uses_utf16_offsets() {
        let f = file("é😀ab");
        let start = ParseLocation::new(f.clone(), 1, 0, 1);
        let end = ParseLocation::new(f, 4, 0, 4);
        let span = ParseSourceSpan::new(start, end);
        assert_eq!(span.to_source_string(), "😀a");
    }

    #[test]
    fn contextual_message_marks_error_start_with_level() {
        let f = file("<p>\n{{ a b }}</p>");
        let loc = ParseLocation::new(f.clone(), 4, 1, 0);
        let span = ParseSourceSpan::new(loc.clone(), loc);
        let err = ParseError::new(span, "Unexpected token b");
        assert_eq!(
            err.contextual_message(),
            "Unexpected token b (\"<p>\n[ERROR ->]{{ a b }}</p>\")"
        );
        assert_eq!(err.level, ParseErrorLevel::Error);
    }

    #[test]
    fn display_includes_location_and_details() {
        let f = file("abc");
        let loc = ParseLocation::new(f, 1, 0, 1);
        let mut span = ParseSourceSpan::new(loc.clone(), loc);
        span.details = Some("detail".to_string());
        let err = ParseError::with_level(span, "msg", ParseErrorLevel::Warning);
        assert_eq!(
            err.to_string(),
            "msg (\"a[WARNING ->]bc\"): test.html@0:1, detail"
        );
    }
}
