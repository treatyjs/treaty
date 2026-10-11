//! Emitter options shared by the oxc emitter ([`super::emitter`]) and the SWC-backend neutral printer
//! ([`super::emitter_swc`]).
//!
//! # `escape_non_ascii`
//!
//! Angular's compliance goldens are printed by the TypeScript printer, and that printer escapes every
//! non-ASCII UTF-16 code unit inside a *string literal* as `\uXXXX` (uppercase hex; an astral character
//! becomes a surrogate pair `🚀`). The i18n placeholder magic values (`�0�`) are the
//! common case. Treaty's default output keeps the raw characters (`string_literal_emits_raw_replacement_char`),
//! so the option is OFF by default and nothing changes for existing users.
//!
//! When ON, both emitters apply TypeScript's `escapeNonAsciiString` to string literals:
//! - a non-ASCII UTF-16 code unit becomes `\uXXXX` (` ` for U+00A0, ` ` for U+2028);
//! - the control-character escapes follow TypeScript's table (`\0`, `\t`, `\n`, `\r`, `\v`, `\f`, `\b`,
//!   `\"`, `\\`), and any other C0 control becomes `\u00XX` (not oxc's `\x07` form);
//! - identifiers, comments, and template-literal raw text are never touched. TypeScript prints a
//!   template's `rawText` verbatim, so non-ASCII in a template body stays raw. Substitutions are
//!   ordinary expressions and are escaped like any other literal.
//!
//! The option is threaded two ways: explicitly ([`super::emitter::emit_statements_with`] and
//! [`super::emitter_swc::emit_statements_with`] take an [`EmitOptions`]), and implicitly for the code
//! paths that call the plain `emit_statements`/`emit_expression` (the facade's compile pipeline), via
//! [`with_emit_options`]. The implicit option is per thread and restored on exit, even on panic.

use std::cell::Cell;

/// Options that change how Ivy output text is printed (not what it means).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmitOptions {
    /// Escape non-ASCII characters in string literals as `\uXXXX`, as the TypeScript printer does.
    /// Default `false`.
    pub escape_non_ascii: bool,
}

thread_local! {
    static CURRENT: Cell<EmitOptions> = const { Cell::new(EmitOptions { escape_non_ascii: false }) };
}

/// The options the plain `emit_statements` / `emit_expression` calls on this thread use.
pub fn current_emit_options() -> EmitOptions {
    CURRENT.with(|c| c.get())
}

/// Run `f` with `opts` as the thread's current emit options, restoring the previous options afterwards
/// (including when `f` panics).
pub fn with_emit_options<R>(opts: EmitOptions, f: impl FnOnce() -> R) -> R {
    struct Restore(EmitOptions);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CURRENT.with(|c| c.replace(opts)));
    f()
}

/// TypeScript's `encodeUtf16EscapeSequence`: `\u` + the UTF-16 code unit as four uppercase hex digits.
pub fn utf16_escape(unit: u16) -> String {
    format!("\\u{unit:04X}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off_and_scoped_option_is_restored() {
        assert!(!current_emit_options().escape_non_ascii);
        with_emit_options(EmitOptions { escape_non_ascii: true }, || {
            assert!(current_emit_options().escape_non_ascii);
        });
        assert!(!current_emit_options().escape_non_ascii);
    }

    #[test]
    fn scoped_option_is_restored_after_a_panic() {
        let result = std::panic::catch_unwind(|| {
            with_emit_options(EmitOptions { escape_non_ascii: true }, || panic!("boom"))
        });
        assert!(result.is_err());
        assert!(!current_emit_options().escape_non_ascii);
    }

    #[test]
    fn utf16_escape_is_uppercase_four_digits() {
        assert_eq!(utf16_escape(0xFFFD), "\\uFFFD");
        assert_eq!(utf16_escape(0x00A0), "\\u00A0");
        assert_eq!(utf16_escape(0xD83D), "\\uD83D");
    }
}
