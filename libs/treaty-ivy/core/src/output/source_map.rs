//! Additive Source Map v3 emission for render3 codegen.
//!
//! # Why this exists
//! The emitter ([`crate::output::emitter`]) lowers the `output_ast` IR into a real
//! `oxc_ast::Program` and prints it with `oxc_codegen`. render3 never tracks
//! lines/columns/spans during emission, and every lowered oxc node is built with the
//! empty `oxc_span::SPAN`, so oxc's own built-in source-map facility has nothing to map
//! FROM. This module provides a self-contained, dependency-free [`SourceMapBuilder`]
//! that accumulates `generated -> original` segments and VLQ-encodes a valid v3 source
//! map.
//!
//! # Byte-identical guarantee
//! Source-map construction is a SEPARATE, ADDITIVE output. It never mutates the printed
//! code string: the position-tracking emit path produces the SAME bytes as the plain
//! `codegen()` path and only records segments on the side. The map is keyed off
//! anchor tokens located in the (already-final) generated code, mapped back to the
//! byte offsets the front-end records on `output_ast` node `meta.span` slots.
//!
//! # Map model
//! A "segment" is a single mapping from a position in the generated file to a position
//! in an original source. We accumulate them in generated order and serialize the v3
//! `mappings` field as base64-VLQ. Coverage is honest: we only emit a segment for a
//! node that actually carries a populated `meta.span` (today the front-end populates the
//! component-level definition span and the class-name token), so an absent span produces
//! NO segment rather than a fabricated one.

use std::fmt::Write as _;

/// A line/column position in a text buffer (both zero-based, matching the Source Map v3
/// convention for the `mappings` field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineCol {
    /// Zero-based line number.
    pub line: u32,
    /// Zero-based column number, counted in UTF-16 code units (the v3 spec's column
    /// unit). For the ASCII-dominated Ivy output this equals the byte/char column; the
    /// helper [`utf16_columns`] converts a byte offset faithfully for the rare non-ASCII
    /// token.
    pub column: u32,
}

impl LineCol {
    pub fn new(line: u32, column: u32) -> Self {
        LineCol { line, column }
    }
}

/// One generated->original mapping segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Segment {
    /// Position in the generated (emitted) file.
    generated: LineCol,
    /// Index into [`SourceMapBuilder::sources`] of the original source this maps to.
    source_index: u32,
    /// Position in that original source.
    original: LineCol,
}

/// Accumulates `generated -> original` segments and VLQ-encodes a valid Source Map v3.
///
/// Construction order:
///   1. [`SourceMapBuilder::new`] with the generated file name.
///   2. [`SourceMapBuilder::add_source`] for each original source (returns its index).
///   3. [`SourceMapBuilder::add_segment`] for each located mapping.
///   4. [`SourceMapBuilder::to_json`] / [`SourceMapBuilder::to_value`] to serialize.
#[derive(Debug, Default)]
pub struct SourceMapBuilder {
    /// The generated file name (the v3 `file` field), if any.
    file: Option<String>,
    /// Original source names (the v3 `sources` field).
    sources: Vec<String>,
    /// Original source contents (the v3 `sourcesContent` field). Index-aligned with
    /// `sources`; an entry is `None` when the content is unknown.
    sources_content: Vec<Option<String>>,
    /// Accumulated mapping segments, in generated order.
    segments: Vec<Segment>,
}

impl SourceMapBuilder {
    /// Start a builder for the given generated file name.
    pub fn new(file: impl Into<String>) -> Self {
        SourceMapBuilder {
            file: Some(file.into()),
            ..SourceMapBuilder::default()
        }
    }

    /// Register an original source (name + optional content). Returns its index, to pass
    /// to [`SourceMapBuilder::add_segment`]. Re-registering the same name returns the
    /// existing index (and fills in content if it was previously unknown).
    pub fn add_source(&mut self, name: impl Into<String>, content: Option<String>) -> u32 {
        let name = name.into();
        if let Some(i) = self.sources.iter().position(|s| *s == name) {
            if self.sources_content[i].is_none() {
                self.sources_content[i] = content;
            }
            return i as u32;
        }
        self.sources.push(name);
        self.sources_content.push(content);
        (self.sources.len() - 1) as u32
    }

    /// Record a mapping from a generated position to an original position in `source_index`.
    pub fn add_segment(
        &mut self,
        generated: LineCol,
        source_index: u32,
        original: LineCol,
    ) {
        self.segments.push(Segment {
            generated,
            source_index,
            original,
        });
    }

    /// Number of accumulated segments (used by tests / honest-coverage reporting).
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// Build the v3 `mappings` string (base64-VLQ). Segments are grouped by generated
    /// line; within a line they are sorted by generated column. Every field is delta-
    /// encoded against the running state required by the v3 spec:
    ///   * generated column resets to 0 at the start of each generated line;
    ///   * source index, original line and original column persist across the WHOLE map.
    fn encode_mappings(&self) -> String {
        // Sort a copy by (generated line, generated column) so output is deterministic
        // regardless of insertion order.
        let mut segs = self.segments.clone();
        segs.sort_by_key(|s| (s.generated.line, s.generated.column));

        let mut out = String::new();
        let mut prev_gen_col: i64 = 0;
        let mut prev_src_index: i64 = 0;
        let mut prev_src_line: i64 = 0;
        let mut prev_src_col: i64 = 0;
        let mut current_line: u32 = 0;
        let mut first_in_line = true;

        for seg in &segs {
            // Emit a `;` for every generated line we advance past, and reset the
            // generated-column delta base (the only field that resets per line).
            while current_line < seg.generated.line {
                out.push(';');
                current_line += 1;
                prev_gen_col = 0;
                first_in_line = true;
            }
            if !first_in_line {
                out.push(',');
            }
            first_in_line = false;

            // Field 1: generated column delta.
            encode_vlq(&mut out, seg.generated.column as i64 - prev_gen_col);
            prev_gen_col = seg.generated.column as i64;
            // Field 2: source index delta.
            encode_vlq(&mut out, seg.source_index as i64 - prev_src_index);
            prev_src_index = seg.source_index as i64;
            // Field 3: original line delta.
            encode_vlq(&mut out, seg.original.line as i64 - prev_src_line);
            prev_src_line = seg.original.line as i64;
            // Field 4: original column delta.
            encode_vlq(&mut out, seg.original.column as i64 - prev_src_col);
            prev_src_col = seg.original.column as i64;
        }
        out
    }

    /// Serialize to a Source Map v3 JSON string. Hand-rolled (no serde dependency) so the
    /// crate gains no new deps; only the small, well-defined v3 shape is produced.
    pub fn to_json(&self) -> String {
        let mut s = String::from("{\"version\":3");
        if let Some(file) = &self.file {
            s.push_str(",\"file\":");
            json_string(&mut s, file);
        }
        s.push_str(",\"sourceRoot\":\"\"");
        s.push_str(",\"sources\":[");
        for (i, src) in self.sources.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            json_string(&mut s, src);
        }
        s.push(']');
        s.push_str(",\"sourcesContent\":[");
        for (i, content) in self.sources_content.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            match content {
                Some(c) => json_string(&mut s, c),
                None => s.push_str("null"),
            }
        }
        s.push(']');
        s.push_str(",\"names\":[]");
        s.push_str(",\"mappings\":");
        json_string(&mut s, &self.encode_mappings());
        s.push('}');
        s
    }
}

/// Append a JSON-escaped string literal (including the surrounding quotes) to `out`.
fn json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Base64-VLQ alphabet (the v3 spec's `A-Za-z0-9+/`).
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Append the base64-VLQ encoding of a signed value to `out` (the Source Map v3
/// variable-length quantity: the sign bit is the LSB, then 5-bit groups with a
/// continuation high bit).
fn encode_vlq(out: &mut String, value: i64) {
    // Move the sign into bit 0.
    let mut vlq: u64 = if value < 0 {
        (((-value) as u64) << 1) | 1
    } else {
        (value as u64) << 1
    };
    loop {
        let mut digit = (vlq & 0b11111) as usize;
        vlq >>= 5;
        if vlq != 0 {
            // More groups follow: set the continuation bit (0x20).
            digit |= 0b100000;
        }
        out.push(BASE64[digit] as char);
        if vlq == 0 {
            break;
        }
    }
}

/// Decode a base64-VLQ `mappings` string into segments, expressed as
/// `(generated_line, generated_column, source_index, original_line, original_column)`
/// absolute tuples. Used by tests to assert a map round-trips; also a faithful reference
/// decoder for the encoder above. Returns `None` on malformed input.
pub fn decode_mappings(mappings: &str) -> Option<Vec<(u32, u32, u32, u32, u32)>> {
    let mut out = Vec::new();
    let mut gen_line: i64 = 0;
    let mut gen_col: i64 = 0;
    let mut src_index: i64 = 0;
    let mut src_line: i64 = 0;
    let mut src_col: i64 = 0;

    for line in split_keep_empty(mappings, ';') {
        gen_col = 0;
        if line.is_empty() {
            gen_line += 1;
            continue;
        }
        for seg in line.split(',') {
            if seg.is_empty() {
                continue;
            }
            let fields = decode_vlq_fields(seg)?;
            // A mapping segment has either 1 field (generated col only) or 4/5 fields.
            match fields.len() {
                1 => {
                    gen_col += fields[0];
                }
                4 | 5 => {
                    gen_col += fields[0];
                    src_index += fields[1];
                    src_line += fields[2];
                    src_col += fields[3];
                    out.push((
                        gen_line as u32,
                        gen_col as u32,
                        src_index as u32,
                        src_line as u32,
                        src_col as u32,
                    ));
                }
                _ => return None,
            }
        }
        gen_line += 1;
    }
    Some(out)
}

/// `str::split` on `sep` but keeps trailing empty segments (so `";;"` yields 3 lines).
fn split_keep_empty(s: &str, sep: char) -> Vec<&str> {
    s.split(sep).collect()
}

/// Decode the consecutive base64-VLQ values in one segment string.
fn decode_vlq_fields(seg: &str) -> Option<Vec<i64>> {
    let mut out = Vec::new();
    let mut shift: u32 = 0;
    let mut acc: i64 = 0;
    for b in seg.bytes() {
        let digit = base64_value(b)? as i64;
        let cont = digit & 0b100000;
        acc += (digit & 0b11111) << shift;
        if cont != 0 {
            shift += 5;
        } else {
            let negate = acc & 1;
            let mut value = acc >> 1;
            if negate != 0 {
                value = -value;
            }
            out.push(value);
            acc = 0;
            shift = 0;
        }
    }
    Some(out)
}

/// Reverse of the base64 alphabet (returns the 6-bit value of a base64 digit byte).
fn base64_value(b: u8) -> Option<u8> {
    BASE64.iter().position(|&c| c == b).map(|i| i as u8)
}

/// Convert a byte offset in `text` into a zero-based (line, UTF-16 column) position.
/// Lines are split on `\n`; the column is the number of UTF-16 code units before the
/// offset on its line. Used to translate the front-end's byte-offset spans into the
/// v3 column unit. An out-of-range offset clamps to the end of the text.
pub fn byte_offset_to_line_col(text: &str, byte_offset: usize) -> LineCol {
    let offset = byte_offset.min(text.len());
    let mut line: u32 = 0;
    let mut line_start_byte: usize = 0;
    for (i, b) in text.as_bytes().iter().enumerate() {
        if i >= offset {
            break;
        }
        if *b == b'\n' {
            line += 1;
            line_start_byte = i + 1;
        }
    }
    let column = utf16_columns(&text[line_start_byte..offset]);
    LineCol::new(line, column)
}

/// UTF-16 code-unit width of a string (the v3 column unit). Astral characters count as 2.
pub fn utf16_columns(s: &str) -> u32 {
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vlq_round_trips_small_values() {
        // Spot-check the canonical examples from the source-map spec.
        for &(value, expected) in &[(0i64, "A"), (1, "C"), (-1, "D"), (16, "gB"), (-16, "hB")] {
            let mut s = String::new();
            encode_vlq(&mut s, value);
            assert_eq!(s, expected, "VLQ({value})");
        }
    }

    #[test]
    fn mappings_encode_and_decode_round_trip() {
        let mut b = SourceMapBuilder::new("out.js");
        let src = b.add_source("comp.ts", Some("source".to_string()));
        // Two segments on generated line 0, one on line 2.
        b.add_segment(LineCol::new(0, 0), src, LineCol::new(3, 5));
        b.add_segment(LineCol::new(0, 10), src, LineCol::new(3, 20));
        b.add_segment(LineCol::new(2, 4), src, LineCol::new(7, 1));

        let mappings = b.encode_mappings();
        let decoded = decode_mappings(&mappings).expect("decode");
        assert_eq!(
            decoded,
            vec![
                (0, 0, 0, 3, 5),
                (0, 10, 0, 3, 20),
                (2, 4, 0, 7, 1),
            ]
        );
    }

    #[test]
    fn to_json_is_valid_v3_shape() {
        let mut b = SourceMapBuilder::new("out.js");
        let src = b.add_source("comp.ts", Some("class C {}".to_string()));
        b.add_segment(LineCol::new(0, 0), src, LineCol::new(0, 6));
        let json = b.to_json();
        assert!(json.contains("\"version\":3"), "{json}");
        assert!(json.contains("\"file\":\"out.js\""), "{json}");
        assert!(json.contains("\"sources\":[\"comp.ts\"]"), "{json}");
        assert!(json.contains("\"sourcesContent\":[\"class C {}\"]"), "{json}");
        assert!(json.contains("\"mappings\":\""), "{json}");
    }

    #[test]
    fn byte_offset_maps_to_line_and_utf16_column() {
        let text = "ab\ncde\nfg";
        assert_eq!(byte_offset_to_line_col(text, 0), LineCol::new(0, 0));
        assert_eq!(byte_offset_to_line_col(text, 1), LineCol::new(0, 1));
        // First byte of line 1 ("cde").
        assert_eq!(byte_offset_to_line_col(text, 3), LineCol::new(1, 0));
        assert_eq!(byte_offset_to_line_col(text, 5), LineCol::new(1, 2));
        // Past the end clamps.
        assert_eq!(byte_offset_to_line_col(text, 999), LineCol::new(2, 2));
    }

    #[test]
    fn add_source_dedups_by_name() {
        let mut b = SourceMapBuilder::new("out.js");
        let a = b.add_source("x.ts", None);
        let again = b.add_source("x.ts", Some("filled".to_string()));
        assert_eq!(a, again, "same name must reuse index");
        assert_eq!(b.sources.len(), 1);
        // Content backfilled on the second call.
        assert_eq!(b.sources_content[0].as_deref(), Some("filled"));
    }
}
