//! `@boundary` / `@error` block parsing. Port of `render3/r3_boundaries.ts`.
//!
//! A `@boundary { … }` block holds its primary content; each connected `@error (…) { … }` block is a
//! fallback branch. An `@error` may declare `let` context variables (`@error (let err, retry =
//! $reset)`), where `$error` and `$reset` are the implicit variables, and a `when <expr>` condition.

use crate::expression::ast::AstNode;
use crate::expression::parser::Parser;
use crate::ml_parser::{self as html, ParseError, ParseSourceSpan as MlSpan};
use crate::template::control_flow::{
    block_spans, match_for_loop_let, move_by, parse_let_parameters, r3_span, span_of,
};
use crate::template::deferred::ChildTransform;
use crate::template::r3_ast as t;

/// `isConnectedBoundaryErrorBlock`: true iff `name === 'error'`.
pub fn is_connected_boundary_error_block(name: &str) -> bool {
    name == "error"
}

/// `WHEN_PATTERN = /^(when\s+)(.*)/`. Returns the captured expression (group 2).
fn match_when(input: &str) -> Option<String> {
    let rest = input.strip_prefix("when")?;
    let first = rest.chars().next()?;
    if !first.is_whitespace() {
        return None;
    }
    let trimmed = rest.trim_start();
    Some(trimmed.split('\n').next().unwrap_or("").to_string())
}

/// `/^\s*[$A-Z_][0-9A-Z_$]*\s*=/i` — an alias-style parameter (`retry = $reset`).
fn is_alias_parameter(input: &str) -> bool {
    let trimmed = input.trim_start();
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(c) if c == '$' || c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    let rest: String = chars
        .clone()
        .take_while(|c| *c == '$' || *c == '_' || c.is_ascii_alphanumeric())
        .collect();
    trimmed[1 + rest.len()..].trim_start().starts_with('=')
}

/// Creates a `@boundary` block from its primary block and connected `@error` blocks. Mirrors
/// `createBoundaryBlock`. The result is always `Some`, as in Angular.
pub fn create_boundary_block(
    ast: &html::Block,
    connected_blocks: &[html::Block],
    transform: &mut ChildTransform<'_>,
    parser: &Parser,
) -> (Option<t::BoundaryBlock>, Vec<ParseError>) {
    let mut errors: Vec<ParseError> = Vec::new();
    // Each error block with its ml-parser source span (used for error reporting).
    let mut error_blocks: Vec<(t::BoundaryErrorBlock, MlSpan)> = Vec::new();

    if !ast.parameters.is_empty() {
        errors.push(ParseError::new(
            ast.source_span.clone(),
            "@boundary block cannot have parameters",
        ));
    }

    for block in connected_blocks {
        if block.name != "error" {
            errors.push(ParseError::new(
                block.source_span.clone(),
                format!("Unrecognized @boundary connected block @{}", block.name),
            ));
            continue;
        }

        // `emptySpan` at the end of the `@error (…) {` start tag: the implicit variables sit there.
        let end_offset = block.start_source_span.end.offset as u32;
        let empty_span = span_of(end_offset, end_offset);
        let mut context_variables: Vec<t::Variable> = vec![
            implicit_variable("$error", &empty_span),
            implicit_variable("$reset", &empty_span),
        ];
        let mut expression: Option<AstNode> = None;

        for param in &block.parameters {
            let let_match = match_for_loop_let(&param.expression);
            if let_match.is_some() || is_alias_parameter(&param.expression) {
                let (expression_to_parse, variables_span) = match &let_match {
                    Some((full, capture)) => {
                        let param_start = param.source_span.start.offset as u32;
                        let full_len = full.chars().count() as i64;
                        let cap_len = capture.chars().count() as i64;
                        (
                            capture.clone(),
                            span_of(
                                move_by(param_start, full_len - cap_len),
                                param.source_span.end.offset as u32,
                            ),
                        )
                    }
                    None => (
                        param.expression.clone(),
                        r3_span(&param.source_span),
                    ),
                };
                parse_let_parameters(
                    &param.source_span,
                    &expression_to_parse,
                    &variables_span,
                    &mut context_variables,
                    &mut errors,
                    &|name, variable_name, source_span, context, errors| {
                        if variable_name != "$error" && variable_name != "$reset" {
                            errors.push(ParseError::new(
                                source_span.clone(),
                                format!(
                                    "Unknown context variable \"{variable_name}\". Only \"$error\" and \"$reset\" are allowed"
                                ),
                            ));
                        } else if context.iter().any(|v| v.name == name) {
                            errors.push(ParseError::new(
                                source_span.clone(),
                                format!("Duplicate \"let\" parameter variable \"{name}\""),
                            ));
                        }
                    },
                    "@error block",
                    Some("$error"),
                );
                continue;
            }

            if let Some(when_expression) = match_when(&param.expression) {
                if expression.is_some() {
                    errors.push(ParseError::new(
                        param.source_span.clone(),
                        "@error block can only have one \"when\" expression",
                    ));
                } else {
                    // `expression.indexOf(whenMatch[2])` locates the condition text after `when `.
                    let start = param.expression.find(&when_expression).unwrap_or(0);
                    let end = start + when_expression.len();
                    let parsed = parser.parse_binding(
                        &param.expression[start..end],
                        r3_span(&param.source_span),
                        (param.source_span.start.offset + start) as i32,
                    );
                    expression = Some(*parsed.ast);
                }
                continue;
            }

            errors.push(ParseError::new(
                param.source_span.clone(),
                format!(
                    "Unrecognized @error block parameter \"{}\"",
                    param.expression
                ),
            ));
        }

        let children = transform(&block.children);
        error_blocks.push((
            t::BoundaryErrorBlock {
                children,
                context_variables,
                expression,
                spans: block_spans(block, r3_span(&block.source_span)),
                i18n: None,
            },
            block.source_span.clone(),
        ));
    }

    // Only one unconditional `@error` is allowed, and it must come last in the chain.
    let mut has_unconditional_error_block = false;
    for (i, (error_block, ml_span)) in error_blocks.iter().enumerate() {
        if error_block.expression.is_some() {
            continue;
        }
        if has_unconditional_error_block {
            errors.push(ParseError::new(
                ml_span.clone(),
                "@boundary block can only have one unconditional @error block",
            ));
        } else if i != error_blocks.len() - 1 {
            errors.push(ParseError::new(
                ml_span.clone(),
                "Unconditional @error block must be the last @error block in the boundary chain",
            ));
        }
        has_unconditional_error_block = true;
    }

    // The whole span runs from the `@boundary` start tag to the end of the last `@error` block.
    let whole_source_span = match error_blocks.last() {
        Some((last, _)) => span_of(
            ast.start_source_span.start.offset as u32,
            last.spans.source_span.end,
        ),
        None => r3_span(&ast.source_span),
    };
    let end_source_span = match error_blocks.last() {
        Some((last, _)) => last.spans.end_source_span.clone(),
        None => ast.end_source_span.as_ref().map(r3_span),
    };

    let children = transform(&ast.children);
    let node = t::BoundaryBlock {
        children,
        error_blocks: error_blocks.into_iter().map(|(b, _)| b).collect(),
        main_block_span: r3_span(&ast.source_span),
        spans: t::BlockSpans {
            name_span: r3_span(&ast.name_span),
            source_span: whole_source_span,
            start_source_span: r3_span(&ast.start_source_span),
            end_source_span,
        },
        i18n: None,
    };

    (Some(node), errors)
}

/// An implicit `$error` / `$reset` context variable (`new t.Variable(name, name, span, span, span)`).
fn implicit_variable(name: &str, span: &crate::expression::ast::ParseSourceSpan) -> t::Variable {
    t::Variable {
        name: name.to_string(),
        value: name.to_string(),
        source_span: span.clone(),
        key_span: span.clone(),
        value_span: Some(span.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml_parser::{parse as html_parse, Node as MlNode};

    /// Parse `src` and split it into the primary block and the blocks connected after it.
    fn boundary_and_connected(src: &str) -> (html::Block, Vec<html::Block>) {
        let result = html_parse(src, "test.html");
        assert!(result.errors.is_empty(), "html parse errors: {:?}", result.errors);
        let mut blocks: Vec<html::Block> = result
            .root_nodes
            .into_iter()
            .filter_map(|n| match n {
                MlNode::Block(b) => Some(*b),
                _ => None,
            })
            .collect();
        let primary = blocks.remove(0);
        (primary, blocks)
    }

    fn no_transform() -> impl FnMut(&[html::Node]) -> Vec<t::Node> {
        |_nodes: &[html::Node]| Vec::new()
    }

    fn run(src: &str) -> (Option<t::BoundaryBlock>, Vec<ParseError>) {
        let (primary, connected) = boundary_and_connected(src);
        let parser = Parser::default();
        let mut transform = no_transform();
        create_boundary_block(&primary, &connected, &mut transform, &parser)
    }

    #[test]
    fn parses_error_blocks_with_implicit_and_declared_variables() {
        let (node, errors) = run("@boundary {x} @error (let err, retry = $reset) {y}");
        assert!(errors.is_empty(), "{errors:?}");
        let node = node.expect("boundary node");
        assert_eq!(node.error_blocks.len(), 1);
        let names: Vec<&str> = node.error_blocks[0]
            .context_variables
            .iter()
            .map(|v| v.name.as_str())
            .collect();
        assert_eq!(names, vec!["$error", "$reset", "err", "retry"]);
        assert_eq!(node.error_blocks[0].context_variables[3].value, "$reset");
        assert!(node.error_blocks[0].expression.is_none());
    }

    #[test]
    fn when_condition_is_parsed_and_unconditional_must_be_last() {
        let (_, errors) = run("@boundary {x} @error {a} @error (when ok) {b}");
        assert!(
            errors
                .iter()
                .any(|e| e.msg == "Unconditional @error block must be the last @error block in the boundary chain"),
            "{errors:?}"
        );

        let (node, errors) = run("@boundary {x} @error (when ok) {b}");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(node.unwrap().error_blocks[0].expression.is_some());
    }

    #[test]
    fn reports_structural_errors() {
        let (_, errors) = run("@boundary (oops) {x} @error {a}");
        assert!(errors.iter().any(|e| e.msg == "@boundary block cannot have parameters"));

        let (_, errors) = run("@boundary {x} @error (nope) {a}");
        assert!(errors.iter().any(|e| e.msg.contains("Unrecognized @error block parameter")));

        let (_, errors) = run("@boundary {x} @error (let e = $bogus) {a}");
        assert!(errors.iter().any(|e| e.msg.contains("Unknown context variable \"$bogus\"")));

        let (_, errors) = run("@boundary {x} @error {p} @error {q}");
        assert!(
            errors
                .iter()
                .any(|e| e.msg == "@boundary block can only have one unconditional @error block"),
            "{errors:?}"
        );
    }

    #[test]
    fn unrecognized_connected_block_is_an_error() {
        let (node, errors) = run("@boundary {x} @placeholder {y}");
        assert!(errors
            .iter()
            .any(|e| e.msg == "Unrecognized @boundary connected block @placeholder"));
        assert!(node.unwrap().error_blocks.is_empty());
    }
}
