//! Leading-comment rendering shared by the oxc emitter ([`super::emitter`]) and the SWC-backend printer
//! ([`super::emitter_swc`]).
//!
//! Angular prints `LeadingComment`s through the TypeScript printer (`ngtsc` translator): a `JSDocComment`
//! is `/*` + its tag block + `*/` (`/**\n * @desc …\n */`), a multi-line plain comment is `/* text */`, and
//! a single-line plain comment is one `// line` per line. A statement's comments each take their own line
//! before the statement; an expression's comments sit inline before its first token (`/* @ts-ignore */
//! goog.getMsg(…)`).
//!
//! Both emitters render through [`comment_lines`], so their bytes match. oxc has no way to print arbitrary
//! text at a node, so the oxc emitter writes a marker (`__treaty_lc<N>__`) and resolves it after codegen
//! (see `emitter::resolve_comment_markers`).

use crate::output_ast::{ExprKind, LeadingComment};
use crate::output_ast as o;

/// The printed lines of one leading comment, without indentation. A multi-line comment has one entry per
/// line (`["/**", " * @desc x", " */"]`), joined by the caller with a newline plus the statement's indent.
pub fn comment_lines(comment: &LeadingComment) -> Vec<String> {
    match comment {
        LeadingComment::JsDoc { .. } => {
            format!("/*{}*/", comment.to_comment_string()).split('\n').map(str::to_string).collect()
        }
        LeadingComment::Plain { text, multiline: true, .. } => {
            format!("/* {text} */").split('\n').map(str::to_string).collect()
        }
        LeadingComment::Plain { text, multiline: false, .. } => {
            text.split('\n').map(|line| format!("// {line}")).collect()
        }
    }
}

/// The text of a statement's comments, one comment after another, as `(lines)` per comment.
pub fn statement_comment_lines(comments: &[LeadingComment]) -> Vec<Vec<String>> {
    comments.iter().map(comment_lines).collect()
}

/// Inline (expression-position) text of one comment: its lines joined by a space, then a trailing space,
/// so `goog.getMsg(…)` reads `/* @ts-ignore */ goog.getMsg(…)`.
pub fn inline_comment_text(comment: &LeadingComment) -> String {
    format!("{} ", comment_lines(comment).join(" "))
}

/// The text an expression's comments print before its leading identifier: each comment's inline text,
/// concatenated, trailing whitespace trimmed (the caller adds the single separating space). Shared by the
/// oxc marker and the swc printer so both emit the same bytes.
pub fn expr_comment_text(comments: &[LeadingComment]) -> String {
    comments.iter().map(inline_comment_text).collect::<String>().trim_end().to_string()
}

/// Whether an expression's first printed token is an identifier (a read of a variable, an external
/// reference, or a member/call chain rooted in one). An expression comment is printed directly before that
/// identifier, so an expression without such a head cannot carry one.
pub fn expr_has_identifier_head(expr: &o::Expr) -> bool {
    match &expr.kind {
        ExprKind::ReadVar { .. } | ExprKind::External { .. } => true,
        ExprKind::Invoke { callee, .. } => {
            !matches!(&callee.kind, ExprKind::Function { .. } | ExprKind::Arrow { .. })
                && expr_has_identifier_head(callee)
        }
        ExprKind::ReadProp { receiver, .. } | ExprKind::ReadKey { receiver, .. } => {
            expr_has_identifier_head(receiver)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_ast::{js_doc_comment, leading_comment, JsDocTag};

    #[test]
    fn jsdoc_renders_as_angular_block_comment() {
        let doc = js_doc_comment(vec![
            JsDocTag { tag_name: Some("desc".into()), text: Some("Hello".into()) },
            JsDocTag { tag_name: Some("meaning".into()), text: Some("greeting".into()) },
        ]);
        assert_eq!(comment_lines(&doc), vec!["/**", " * @desc Hello", " * @meaning greeting", " */"]);
    }

    #[test]
    fn single_jsdoc_tag_uses_one_line_form() {
        let doc = js_doc_comment(vec![JsDocTag {
            tag_name: Some("suppress".into()),
            text: None,
        }]);
        assert_eq!(comment_lines(&doc), vec!["/** @suppress */"]);
    }

    #[test]
    fn multiline_plain_comment_is_one_block() {
        assert_eq!(comment_lines(&leading_comment("@ts-ignore", true, true)), vec!["/* @ts-ignore */"]);
    }

    #[test]
    fn single_line_plain_comment_is_one_line_per_line() {
        assert_eq!(comment_lines(&leading_comment("a\nb", false, true)), vec!["// a", "// b"]);
    }

    #[test]
    fn inline_text_is_followed_by_a_space() {
        assert_eq!(inline_comment_text(&leading_comment("@ts-ignore", true, true)), "/* @ts-ignore */ ");
    }
}
