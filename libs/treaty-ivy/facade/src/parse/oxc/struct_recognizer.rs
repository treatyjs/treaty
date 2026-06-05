//! The TC39-structs RECOGNIZER + bridge-rewriter — a Treaty-owned pre-pass that lets the oxc parse
//! backend ingest `struct`/`shared struct` declarations WITHOUT forking oxc.
//!
//! oxc 0.133 has no `StructDeclaration` AST node (the parser would mis-handle a leading `struct`
//! identifier in statement position). Rather than fork oxc — which would break the mechanical-bump
//! invariant of `migration/OXC-MIGRATION-HARNESS.md` — this module:
//!
//!   1. **Recognizes** `struct Name { … }` and `shared struct Name { … }` declarations in STATEMENT
//!      position, recording each one's [`StructKind`], its declared NAME, and its declared FIELD names.
//!   2. **Bridge-rewrites** the recognized keyword to `class` by overwriting it (and, for
//!      `shared struct`, the `shared` prefix) with SPACES + `class`, keeping the byte length IDENTICAL
//!      so every span inside the class body — names, members, decorators — is preserved exactly. oxc
//!      then parses the rewritten source as an ordinary `class`, byte-for-byte where the body is
//!      concerned.
//!   3. The oxc backend merges each recognized declaration's [`StructKind`] onto the matching neutral
//!      [`ClassWithDecorators`] (by NAME span) after lowering, so the only observable difference from a
//!      plain class is the additive `struct_kind` field.
//!
//! # Lexical correctness (the load-bearing risk)
//!
//! `struct` is a perfectly legal identifier today and `shared` is contextual, so a naive text rewrite
//! would corrupt code like `let struct = 1;` or `shared.foo`. This scanner is therefore a real
//! mini-lexer: it skips line/block comments, single/double-quoted and template strings (incl. `${…}`
//! interpolations), and regex literals, and it only treats `struct`/`shared struct` as a declaration
//! keyword when it appears at a STATEMENT BOUNDARY (start of input, or right after `{`/`}`/`;`, or
//! right after an `export` / `export default` prefix) and is immediately followed — with NO
//! intervening line terminator (the proposal's `[no LineTerminator here]` ASI rule) — by an
//! identifier name and then a `{` (optionally via `extends <Ident>`). Anything else is left untouched.
//!
//! This is intentionally CONSERVATIVE: it recognizes the declaration FORMS the proposal specifies and
//! declines every ambiguous case, never rewriting a `struct`/`shared` that could be an identifier.

use super::super::StructKind;

/// One recognized struct declaration: which kind, its declared name, and its declared field names (in
/// source order). The field list is best-effort metadata for later lowering; the parity test asserts
/// it, and a struct with no plain fields simply carries an empty list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RecognizedStruct {
    /// `Struct` for an unshared `struct`, `SharedStruct` for a `shared struct`.
    pub kind: StructKind,
    /// The declared type name (`struct Box` → `"Box"`).
    pub name: String,
    /// The declared instance FIELD names in source order (`x;`/`y = 0;` → `["x", "y"]`). Methods,
    /// getters/setters and the constructor are NOT fields and are excluded.
    pub fields: Vec<String>,
}

/// The result of the pre-pass: the bridge-rewritten source (struct/shared keywords overwritten with
/// `class`, byte length preserved) plus the recognized declarations in source order.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct StructPreScan {
    /// The source to hand to oxc — identical to the input except recognized `struct`/`shared struct`
    /// keywords are replaced by `class` (space-padded so all later byte offsets are unchanged). When
    /// no struct is recognized this equals the input.
    pub rewritten: String,
    /// The recognized struct declarations, in source order.
    pub structs: Vec<RecognizedStruct>,
}

impl StructPreScan {
    /// Whether the pre-scan found any struct declaration (so the caller can skip the merge fast-path).
    pub fn is_empty(&self) -> bool {
        self.structs.is_empty()
    }
}

/// Pre-scan `source` for `struct`/`shared struct` declarations, returning the bridge-rewritten source
/// + the recognized declarations. When no struct keyword is recognized the rewritten source is the
/// input unchanged and `structs` is empty (the common case — zero cost beyond one lexical scan).
pub(crate) fn pre_scan(source: &str) -> StructPreScan {
    let bytes = source.as_bytes();
    // Byte ranges to blank out (overwrite with spaces) and the byte index at which to write `class`.
    // Collected first so the rewrite is a single pass over an owned copy.
    let mut rewrites: Vec<KeywordRewrite> = Vec::new();
    let mut structs: Vec<RecognizedStruct> = Vec::new();

    let mut scanner = Scanner::new(source);
    while let Some(tok) = scanner.next_token() {
        // A struct declaration may only begin at a statement boundary.
        if !tok.at_statement_start {
            continue;
        }
        // Two declaration shapes: `struct …` and `shared struct …`. `shared`/`struct` are contextual,
        // so they only count when the precise declaration shape follows with no line terminator.
        let (kind, keyword_start) = if tok.is_word(source, "struct") {
            (StructKind::Struct, tok.start)
        } else if tok.is_word(source, "shared") {
            // `shared` must be IMMEDIATELY followed (no LineTerminator) by `struct`.
            match scanner.peek_keyword_no_newline("struct") {
                Some(_struct_tok) => (StructKind::SharedStruct, tok.start),
                None => continue,
            }
        } else {
            continue;
        };

        // For `shared struct`, consume the `struct` token now (it was only peeked above).
        if kind == StructKind::SharedStruct {
            scanner.next_token();
        }

        // `[no LineTerminator here]` then the declared NAME identifier.
        let Some(name_tok) = scanner.next_word_no_newline() else {
            continue;
        };
        let name = name_tok.word(source).to_owned();

        // Optionally `extends <Ident>` (structs may only extend structs / shared structs only shared);
        // we accept and skip it — the body still parses as a class either way.
        let after_name = scanner.clone_position();
        if let Some(kw) = scanner.peek_word() {
            if kw.is_word(source, "extends") {
                scanner.next_token(); // extends
                // Skip the (possibly qualified) superclass reference up to the opening `{`.
                scanner.skip_until_brace_or_semi();
            } else {
                scanner.restore_position(after_name);
            }
        }

        // The class body must open with `{`. If it does not, this was not a struct declaration shape
        // (e.g. `struct` used as a value); decline without rewriting.
        let Some(body_open) = scanner.expect_open_brace() else {
            continue;
        };

        // Extract the declared instance field names from the body (best-effort, lexical).
        let fields = scan_field_names(source, body_open, &mut scanner);

        // Record the keyword rewrite: overwrite [keyword_start .. name_tok.start) with spaces, then
        // write `class` immediately before the name (padded so the name's offset never moves).
        rewrites.push(KeywordRewrite {
            blank_from: keyword_start,
            name_start: name_tok.start,
        });
        structs.push(RecognizedStruct { kind, name, fields });
    }

    if rewrites.is_empty() {
        return StructPreScan {
            rewritten: source.to_owned(),
            structs,
        };
    }

    let mut out = bytes.to_vec();
    for rw in &rewrites {
        // Blank the whole `[keyword .. name)` gap to spaces, then stamp `class` at the START of the
        // gap, leaving the remaining bytes as spaces up to the name. The gap is always >= 7 bytes
        // (`struct ` is 7, `shared struct ` is 14), so `class` (5) always fits and the name keeps its
        // exact byte offset — every span inside the class body is preserved.
        for b in &mut out[rw.blank_from..rw.name_start] {
            *b = b' ';
        }
        let class = b"class";
        out[rw.blank_from..rw.blank_from + class.len()].copy_from_slice(class);
    }

    StructPreScan {
        // SAFETY/UTF-8: we only ever overwrite ASCII keyword bytes with ASCII (`class`/space) inside a
        // run we proved is the ASCII `struct`/`shared struct` keyword + ASCII whitespace, so the result
        // is still valid UTF-8. `from_utf8` validates regardless, so we fall back to the input on the
        // impossible failure rather than panic.
        rewritten: String::from_utf8(out).unwrap_or_else(|_| source.to_owned()),
        structs,
    }
}

/// A pending keyword rewrite: blank `[blank_from .. name_start)` and stamp `class` ending just before
/// `name_start`.
struct KeywordRewrite {
    blank_from: usize,
    name_start: usize,
}

/// Scan the declared instance FIELD names out of a class body that opens at `body_open` (the index of
/// the `{`). Best-effort + lexical: a field is a `name`/`name = …`/`name;` at brace DEPTH 1 that is
/// NOT immediately followed by `(` (a method) and is not `constructor`/`get`/`set`/`static` keywords.
/// This is metadata only — the body still parses as a class regardless of what we collect here.
fn scan_field_names(source: &str, body_open: usize, scanner: &mut Scanner) -> Vec<String> {
    // Re-scan the body region independently so the outer scanner position is unaffected by depth
    // bookkeeping; the outer scanner only needs to skip PAST the matching close brace.
    let mut fields = Vec::new();
    let mut inner = Scanner::at(source, body_open + 1);
    let mut brace = 1usize; // `{}` nesting; we are inside the opening `{`.
    let mut paren = 0usize; // `()` / `[]` nesting — a field name lives at paren depth 0 only.
    // The candidate field name: the last bare identifier seen at brace-depth 1 / paren-depth 0 that
    // has NOT been disqualified by a following `(` (a method) or `{` (a method/getter body).
    let mut pending: Option<Word> = None;

    while let Some(tok) = inner.next_token() {
        match tok.kind {
            TokKind::Punct(b'{') => {
                brace += 1;
                pending = None; // a `{` after a name → method/getter body, not a field.
            }
            TokKind::Punct(b'}') => {
                brace -= 1;
                if brace == 0 {
                    break;
                }
                pending = None;
            }
            TokKind::Punct(b'(') | TokKind::Punct(b'[') => {
                paren += 1;
                pending = None; // a `(`/`[` after a name → method params / computed key, not a field.
            }
            TokKind::Punct(b')') | TokKind::Punct(b']') => {
                paren = paren.saturating_sub(1);
            }
            TokKind::Word(w) if brace == 1 && paren == 0 => {
                let text = w.word(source);
                // Member modifiers / accessor keywords are not the field NAME — the next plain word is.
                if matches!(
                    text,
                    "static" | "get" | "set" | "constructor" | "async" | "readonly" | "declare"
                ) {
                    pending = None;
                    continue;
                }
                pending = Some(w);
            }
            // `;` or `=` at the member level CONFIRMS the pending identifier was a field (`x;` / `x = …`).
            TokKind::Punct(b';') | TokKind::Punct(b'=') if brace == 1 && paren == 0 => {
                if let Some(prev) = pending.take() {
                    fields.push(prev.word(source).to_owned());
                }
            }
            _ => {}
        }
    }
    // A trailing field with no terminator before `}` (`struct S { x }`) is still a field.
    if let Some(prev) = pending.take() {
        fields.push(prev.word(source).to_owned());
    }

    // Advance the OUTER scanner past the matching close brace so the main loop resumes after the body.
    scanner.skip_balanced_braces_from(body_open);
    fields
}

// ===========================================================================
// A small JS-aware lexical scanner: enough to find declaration keywords without being fooled by
// strings / comments / regex / templates, and to track statement-boundary context.
// ===========================================================================

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TokKind {
    /// An identifier / keyword word.
    Word(Word),
    /// A single punctuation byte we care about (`{`, `}`, `;`, `(`, `)`, `=`, …).
    Punct(u8),
}

/// A word token's byte span (so we can recover its text against the source without copying).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Word {
    start: usize,
    end: usize,
}

impl Word {
    fn word<'s>(self, source: &'s str) -> &'s str {
        &source[self.start..self.end]
    }
}

/// A scanned token plus the lexical context the recognizer needs.
#[derive(Clone, Copy, Debug)]
struct Token {
    kind: TokKind,
    start: usize,
    end: usize,
    /// Whether this token sits at a statement boundary (start of input, or after `{`/`}`/`;`, or after
    /// an `export`/`default` keyword prefix).
    at_statement_start: bool,
    /// Whether a line terminator appeared in the whitespace/comments immediately BEFORE this token.
    newline_before: bool,
}

impl Token {
    /// The token's verbatim source text.
    fn word<'s>(&self, source: &'s str) -> &'s str {
        &source[self.start..self.end]
    }

    /// Whether this token is the WORD `w` (an exact identifier-text match). Punctuation tokens never
    /// match a word.
    fn is_word(&self, source: &str, w: &str) -> bool {
        matches!(self.kind, TokKind::Word(_)) && self.word(source) == w
    }
}

/// The lexer over a source string.
#[derive(Clone)]
struct Scanner<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    /// The statement-boundary flag for the NEXT token (set by the previous token / boundary punct).
    next_is_stmt_start: bool,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            next_is_stmt_start: true, // start of input is a statement boundary.
        }
    }

    fn at(src: &'a str, pos: usize) -> Self {
        Self {
            src,
            bytes: src.as_bytes(),
            pos,
            next_is_stmt_start: true,
        }
    }

    fn clone_position(&self) -> ScannerPos {
        ScannerPos {
            pos: self.pos,
            next_is_stmt_start: self.next_is_stmt_start,
        }
    }

    fn restore_position(&mut self, p: ScannerPos) {
        self.pos = p.pos;
        self.next_is_stmt_start = p.next_is_stmt_start;
    }

    /// Skip whitespace, line + block comments, returning whether a line terminator was crossed.
    fn skip_trivia(&mut self) -> bool {
        let mut newline = false;
        loop {
            let Some(&b) = self.bytes.get(self.pos) else {
                return newline;
            };
            match b {
                b'\n' | b'\r' => {
                    newline = true;
                    self.pos += 1;
                }
                b' ' | b'\t' | 0x0c | 0x0b => self.pos += 1,
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                    self.pos += 2;
                    while let Some(&c) = self.bytes.get(self.pos) {
                        if c == b'\n' || c == b'\r' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                b'/' if self.bytes.get(self.pos + 1) == Some(&b'*') => {
                    self.pos += 2;
                    while self.pos < self.bytes.len() {
                        if self.bytes[self.pos] == b'\n' || self.bytes[self.pos] == b'\r' {
                            newline = true;
                        }
                        if self.bytes[self.pos] == b'*'
                            && self.bytes.get(self.pos + 1) == Some(&b'/')
                        {
                            self.pos += 2;
                            break;
                        }
                        self.pos += 1;
                    }
                }
                _ => return newline,
            }
        }
    }

    /// Produce the next significant token (word or punctuation), skipping over string / template /
    /// regex literals (whose contents must never be interpreted as code). Returns `None` at EOF.
    fn next_token(&mut self) -> Option<Token> {
        loop {
            let newline_before = self.skip_trivia();
            let start = self.pos;
            let &b = self.bytes.get(self.pos)?;

            // String + template literals: consume whole, then continue (they yield no recognizer token,
            // but they DO end any statement-start context — an expression follows).
            if b == b'"' || b == b'\'' {
                self.skip_quoted(b);
                self.next_is_stmt_start = false;
                continue;
            }
            if b == b'`' {
                self.skip_template();
                self.next_is_stmt_start = false;
                continue;
            }
            // Regex vs divide: a `/` in statement/expression-start position begins a regex literal.
            if b == b'/' {
                // (Comments already handled in skip_trivia.) Treat as regex when we are at a position
                // where an expression may start; otherwise it is a division operator.
                if self.next_is_stmt_start {
                    self.skip_regex();
                    self.next_is_stmt_start = false;
                    continue;
                }
                self.pos += 1;
                self.next_is_stmt_start = false;
                continue;
            }

            // Identifier / keyword word.
            if is_ident_start(b) {
                let at_statement_start = self.next_is_stmt_start;
                while let Some(&c) = self.bytes.get(self.pos) {
                    if is_ident_part(c) {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                let end = self.pos;
                let word = Word { start, end };
                // After a word, the next token is at a statement boundary only if THIS word is an
                // `export`/`default` declaration prefix; otherwise we are mid-expression.
                let text = &self.src[start..end];
                self.next_is_stmt_start = matches!(text, "export" | "default");
                return Some(Token {
                    kind: TokKind::Word(word),
                    start,
                    end,
                    at_statement_start,
                    newline_before,
                });
            }

            // A single punctuation byte we may care about.
            let at_statement_start = self.next_is_stmt_start;
            self.pos += 1;
            // Statement boundaries: after `{` / `}` / `;` a new statement may begin. After `=`/`(`/`,`
            // an expression begins (so a `/` is a regex, but `struct` would be an identifier).
            self.next_is_stmt_start = matches!(b, b'{' | b'}' | b';');
            return Some(Token {
                kind: TokKind::Punct(b),
                start,
                end: self.pos,
                at_statement_start,
                newline_before,
            });
        }
    }

    /// Peek the next WORD token (without consuming) if the next significant token is a word.
    fn peek_word(&self) -> Option<Token> {
        let mut probe = self.clone();
        match probe.next_token() {
            Some(tok) if matches!(tok.kind, TokKind::Word(_)) => Some(tok),
            _ => None,
        }
    }

    /// Peek the next token and return it only if it is the word `expected` AND no line terminator
    /// precedes it (the `[no LineTerminator here]` rule). Does NOT consume.
    fn peek_keyword_no_newline(&self, expected: &str) -> Option<Token> {
        let mut probe = self.clone();
        match probe.next_token() {
            Some(tok)
                if !tok.newline_before
                    && matches!(tok.kind, TokKind::Word(_))
                    && tok.word(self.src) == expected =>
            {
                Some(tok)
            }
            _ => None,
        }
    }

    /// Consume + return the next token if it is a WORD with NO preceding line terminator; else `None`
    /// (and the position is left unmoved so the caller can decline cleanly).
    fn next_word_no_newline(&mut self) -> Option<Token> {
        let save = self.clone_position();
        match self.next_token() {
            Some(tok) if !tok.newline_before && matches!(tok.kind, TokKind::Word(_)) => Some(tok),
            _ => {
                self.restore_position(save);
                None
            }
        }
    }

    /// Expect the next significant token to be `{`; consume + return its byte index, else `None`
    /// (position restored).
    fn expect_open_brace(&mut self) -> Option<usize> {
        let save = self.clone_position();
        match self.next_token() {
            Some(tok) if matches!(tok.kind, TokKind::Punct(b'{')) => Some(tok.start),
            _ => {
                self.restore_position(save);
                None
            }
        }
    }

    /// Skip tokens up to — but NOT including — the next `{` or `;`, used to step over an
    /// `extends <Ref>` clause. The caller's later `expect_open_brace` then re-reads the `{`.
    fn skip_until_brace_or_semi(&mut self) {
        loop {
            let save = self.clone_position();
            match self.next_token() {
                Some(tok) if matches!(tok.kind, TokKind::Punct(b'{') | TokKind::Punct(b';')) => {
                    // Stop right before the boundary so `expect_open_brace` sees it.
                    self.restore_position(save);
                    return;
                }
                Some(_) => continue,
                None => return,
            }
        }
    }

    /// From the `{` at `open`, advance THIS scanner past the matching `}` (depth-balanced, string /
    /// comment / regex aware), so the main loop resumes after the class body.
    fn skip_balanced_braces_from(&mut self, open: usize) {
        let mut inner = Scanner::at(self.src, open + 1);
        let mut depth = 1usize;
        while let Some(tok) = inner.next_token() {
            match tok.kind {
                TokKind::Punct(b'{') => depth += 1,
                TokKind::Punct(b'}') => {
                    depth -= 1;
                    if depth == 0 {
                        self.pos = inner.pos;
                        self.next_is_stmt_start = true;
                        return;
                    }
                }
                _ => {}
            }
        }
        self.pos = inner.pos;
        self.next_is_stmt_start = true;
    }

    fn skip_quoted(&mut self, quote: u8) {
        self.pos += 1; // opening quote
        while let Some(&b) = self.bytes.get(self.pos) {
            self.pos += 1;
            if b == b'\\' {
                self.pos += 1; // skip the escaped byte
            } else if b == quote {
                return;
            }
        }
    }

    fn skip_template(&mut self) {
        self.pos += 1; // opening backtick
        while let Some(&b) = self.bytes.get(self.pos) {
            if b == b'\\' {
                self.pos += 2;
                continue;
            }
            if b == b'`' {
                self.pos += 1;
                return;
            }
            // `${ … }` interpolation: skip to the matching `}` (depth-balanced over nested templates).
            if b == b'$' && self.bytes.get(self.pos + 1) == Some(&b'{') {
                self.pos += 2;
                let mut depth = 1usize;
                while let Some(&c) = self.bytes.get(self.pos) {
                    match c {
                        b'{' => depth += 1,
                        b'}' => {
                            depth -= 1;
                            if depth == 0 {
                                self.pos += 1;
                                break;
                            }
                        }
                        b'`' => {
                            // A nested template inside the interpolation.
                            self.skip_template();
                            continue;
                        }
                        _ => {}
                    }
                    self.pos += 1;
                }
                continue;
            }
            self.pos += 1;
        }
    }

    fn skip_regex(&mut self) {
        self.pos += 1; // opening slash
        let mut in_class = false; // inside a `[…]` char class, where `/` is literal.
        while let Some(&b) = self.bytes.get(self.pos) {
            self.pos += 1;
            match b {
                b'\\' => self.pos += 1,
                b'[' => in_class = true,
                b']' => in_class = false,
                b'/' if !in_class => {
                    // Skip flags.
                    while let Some(&f) = self.bytes.get(self.pos) {
                        if is_ident_part(f) {
                            self.pos += 1;
                        } else {
                            break;
                        }
                    }
                    return;
                }
                b'\n' | b'\r' => return, // unterminated; bail.
                _ => {}
            }
        }
    }
}

/// A saved scanner position for backtracking.
#[derive(Clone, Copy)]
struct ScannerPos {
    pos: usize,
    next_is_stmt_start: bool,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

fn is_ident_part(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(scan: &StructPreScan) -> Vec<(&str, StructKind)> {
        scan.structs
            .iter()
            .map(|s| (s.name.as_str(), s.kind))
            .collect()
    }

    #[test]
    fn recognizes_plain_struct() {
        let src = "struct Box { x; y; constructor(x, y) { this.x = x; } sum() { return 1; } }";
        let scan = pre_scan(src);
        assert_eq!(names(&scan), vec![("Box", StructKind::Struct)]);
        assert_eq!(scan.structs[0].fields, vec!["x", "y"]);
        // Bridge rewrite: `struct ` (7 bytes) → `class  ` — keyword overwritten, byte length preserved,
        // name offset unchanged.
        assert_eq!(scan.rewritten.len(), src.len());
        assert!(scan.rewritten.starts_with("class  Box {"), "{}", scan.rewritten);
        assert_eq!(scan.rewritten.find("Box"), src.find("Box"));
    }

    #[test]
    fn recognizes_shared_struct() {
        let scan = pre_scan("shared struct SharedBox { x; y; }");
        assert_eq!(names(&scan), vec![("SharedBox", StructKind::SharedStruct)]);
        assert_eq!(scan.structs[0].fields, vec!["x", "y"]);
        // `shared struct ` (14 bytes before the name) → `class` + spaces, name offset kept exactly.
        assert_eq!(scan.rewritten.find("SharedBox"), "shared struct SharedBox".find("SharedBox"));
        assert!(scan.rewritten.starts_with("class"), "{}", scan.rewritten);
        // The body after the name is byte-identical to the input.
        let at = "shared struct SharedBox".find("SharedBox").unwrap();
        assert_eq!(&scan.rewritten[at..], &"shared struct SharedBox { x; y; }"[at..]);
    }

    #[test]
    fn struct_as_identifier_is_not_rewritten() {
        // `struct`/`shared` as ordinary identifiers must be left untouched.
        for src in [
            "let struct = 1;",
            "const x = struct.field;",
            "foo(struct);",
            "shared.foo();",
            "const shared = 2; shared + struct;",
            "return struct;",
        ] {
            let scan = pre_scan(src);
            assert!(scan.is_empty(), "must not recognize a struct in: {src}");
            assert_eq!(scan.rewritten, src, "must not rewrite: {src}");
        }
    }

    #[test]
    fn struct_keyword_in_string_or_comment_is_ignored() {
        for src in [
            r#"const s = "struct Box { x; }";"#,
            "// struct Box { x; }\nconst y = 1;",
            "/* struct Box {} */ const z = 2;",
            "const t = `struct ${x} Box`;",
        ] {
            let scan = pre_scan(src);
            assert!(scan.is_empty(), "must not recognize inside literal/comment: {src}");
            assert_eq!(scan.rewritten, src);
        }
    }

    #[test]
    fn exported_struct_is_recognized() {
        let scan = pre_scan("export struct E { a; }");
        assert_eq!(names(&scan), vec![("E", StructKind::Struct)]);
        assert_eq!(scan.structs[0].fields, vec!["a"]);
        // `export struct E` → `export class  E` (keyword overwritten, name offset preserved).
        assert!(scan.rewritten.contains("class"), "{}", scan.rewritten);
        assert_eq!(scan.rewritten.find("E {"), "export struct E {".find("E {"));
        assert_eq!(scan.rewritten.len(), "export struct E { a; }".len());
    }

    #[test]
    fn struct_extends_struct_is_recognized() {
        let scan = pre_scan("struct P3 extends P { z; constructor(x, z) { super(x); } }");
        assert_eq!(names(&scan), vec![("P3", StructKind::Struct)]);
        assert_eq!(scan.structs[0].fields, vec!["z"]);
        assert!(scan.rewritten.contains("class  P3 extends P"), "{}", scan.rewritten);
    }

    #[test]
    fn shared_then_newline_struct_is_not_a_shared_struct() {
        // `shared` then a LINE TERMINATOR then `struct` violates `[no LineTerminator here]`: `shared`
        // is an identifier/ASI statement, and a bare `struct …` on the next line is its own struct.
        let scan = pre_scan("shared\nstruct S { x; }");
        // The decisive invariant: a line terminator between `shared` and `struct` must NEVER form a
        // `shared struct` (the proposal's `[no LineTerminator here]` rule). The recognizer is
        // conservative here — it declines the whole construct rather than risk corrupting a `shared`
        // identifier statement — so no SharedStruct is produced.
        assert!(
            scan.structs.iter().all(|s| s.kind != StructKind::SharedStruct),
            "a newline between shared and struct must not form a shared struct: {:?}",
            scan.structs
        );
    }

    #[test]
    fn rewrite_preserves_byte_length_and_body_offsets() {
        let src = "struct Box { x; y; foo() { return this.x; } }";
        let scan = pre_scan(src);
        assert_eq!(scan.rewritten.len(), src.len(), "byte length must be preserved");
        // Every byte from the name onward is unchanged.
        let name_at = src.find("Box").unwrap();
        assert_eq!(&scan.rewritten[name_at..], &src[name_at..], "body must be byte-identical");
    }

    #[test]
    fn multiple_structs_in_one_source() {
        let src = "struct A { a; } shared struct B { b; } class C {}";
        let scan = pre_scan(src);
        assert_eq!(
            names(&scan),
            vec![("A", StructKind::Struct), ("B", StructKind::SharedStruct)]
        );
    }
}
