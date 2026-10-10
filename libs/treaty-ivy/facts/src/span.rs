//! UTF-16 <-> byte offset conversion.
//!
//! Angular's Rust analyser reports every span in UTF-16 code units (the JavaScript string index
//! space); Treaty's front-end works in UTF-8 byte offsets. The two agree for ASCII sources only, so
//! every span that crosses the bridge goes through [`Utf16Index`].

/// A half-open byte range `[start, end)` into the UTF-8 source text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Whether `other` lies entirely inside this span.
    pub fn contains(&self, other: Span) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

/// Maps UTF-16 code-unit offsets of one source text to UTF-8 byte offsets, and back.
///
/// Built once per source. ASCII-only sources take a fast path (both offsets are identical).
#[derive(Debug, Clone)]
pub struct Utf16Index {
    /// `None` for an ASCII-only source (identity mapping).
    table: Option<Vec<usize>>,
    byte_len: usize,
}

impl Utf16Index {
    pub fn new(source: &str) -> Self {
        if source.is_ascii() {
            return Self { table: None, byte_len: source.len() };
        }
        // `table[u]` = byte offset of the UTF-16 code unit `u`. A surrogate pair maps its low half to
        // the byte offset of the code point's start, so a span ending inside a pair still lands on a
        // char boundary.
        let mut table = Vec::with_capacity(source.len() + 1);
        for (byte, ch) in source.char_indices() {
            table.push(byte);
            if ch.len_utf16() == 2 {
                table.push(byte);
            }
        }
        table.push(source.len());
        Self { table: Some(table), byte_len: source.len() }
    }

    /// Byte offset of a UTF-16 offset, or `None` when it is past the end of the source.
    pub fn to_byte(&self, utf16: u32) -> Option<usize> {
        match &self.table {
            None => {
                let u = utf16 as usize;
                (u <= self.byte_len).then_some(u)
            }
            Some(table) => table.get(utf16 as usize).copied(),
        }
    }

    /// UTF-16 offset of a byte offset (which must sit on a char boundary).
    pub fn to_utf16(&self, source: &str, byte: usize) -> Option<u32> {
        if byte > source.len() || !source.is_char_boundary(byte) {
            return None;
        }
        Some(source[..byte].encode_utf16().count() as u32)
    }

    /// Converts a UTF-16 span to a byte span. `None` when either end is out of range or the span is
    /// inverted.
    pub fn span(&self, start: u32, end: u32) -> Option<Span> {
        if end < start {
            return None;
        }
        Some(Span::new(self.to_byte(start)?, self.to_byte(end)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_source_is_identity() {
        let src = "@Component({selector: 'a'}) class A {}";
        let idx = Utf16Index::new(src);
        assert_eq!(idx.span(0, 9), Some(Span::new(0, 9)));
        assert_eq!(idx.to_byte(src.len() as u32), Some(src.len()));
        assert_eq!(idx.to_byte(src.len() as u32 + 1), None);
    }

    #[test]
    fn multibyte_characters_shift_offsets() {
        // 'é' is 2 bytes and 1 UTF-16 unit; '𝄞' is 4 bytes and 2 UTF-16 units.
        let src = "é𝄞x";
        let idx = Utf16Index::new(src);
        assert_eq!(idx.to_byte(0), Some(0));
        assert_eq!(idx.to_byte(1), Some(2)); // start of 𝄞
        assert_eq!(idx.to_byte(3), Some(6)); // start of 'x'
        assert_eq!(idx.to_byte(4), Some(7)); // end of source
        assert_eq!(idx.span(1, 3), Some(Span::new(2, 6)));
        assert_eq!(&src[2..6], "𝄞");
    }

    #[test]
    fn round_trip_on_char_boundaries() {
        let src = "ünïcødé ✓ 𝄞 end";
        let idx = Utf16Index::new(src);
        for (byte, _) in src.char_indices().chain([(src.len(), ' ')]) {
            let utf16 = idx.to_utf16(src, byte).unwrap();
            assert_eq!(idx.to_byte(utf16), Some(byte), "byte {byte}");
        }
    }

    #[test]
    fn inverted_and_out_of_range_spans_are_rejected() {
        let idx = Utf16Index::new("aé");
        assert_eq!(idx.span(2, 1), None);
        assert_eq!(idx.span(0, 99), None);
        assert_eq!(idx.to_utf16("aé", 2), None, "inside the 2-byte char");
    }

    #[test]
    fn span_containment() {
        assert!(Span::new(0, 10).contains(Span::new(2, 5)));
        assert!(!Span::new(0, 10).contains(Span::new(5, 11)));
    }
}
