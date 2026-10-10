//! `@content` block parsing. Port of `render3/r3_content_blocks.ts`.
//!
//! `@content(name)` / `@content(name; let a, b)` declares a slot inside a foreign component element
//! whose body is rendered by the foreign framework. The `let` variables map positionally onto the
//! arguments passed to the render call.

use crate::ml_parser::{self as html, ParseError, ParseSourceSpan as MlSpan};
use crate::template::control_flow::{block_spans, is_identifier, match_for_loop_let, r3_span};
use crate::template::deferred::ChildTransform;
use crate::template::r3_ast as t;

/// Creates a `@content` block. Mirrors `createContentBlock`: `None` (with errors) when the parameters
/// are malformed, otherwise the block (with any variable errors reported alongside it).
pub fn create_content_block(
    ast: &html::Block,
    transform: &mut ChildTransform<'_>,
) -> (Option<t::ContentBlock>, Vec<ParseError>) {
    let mut errors: Vec<ParseError> = Vec::new();

    if ast.parameters.is_empty() || ast.parameters.len() > 2 {
        errors.push(ParseError::new(
            ast.start_source_span.clone(),
            "@content block must have one or two parameters, e.g. \"@content(header)\" or \"@content(items; let item, index)\"",
        ));
        return (None, errors);
    }

    let name_param = &ast.parameters[0];
    let name = name_param.expression.trim().to_string();
    if name.contains(',') {
        errors.push(ParseError::new(
            ast.start_source_span.clone(),
            "@content block must have exactly one name parameter",
        ));
        return (None, errors);
    }

    if !is_identifier(&name) {
        errors.push(ParseError::new(
            name_param.source_span.clone(),
            "@content name must be a valid JavaScript identifier",
        ));
        return (None, errors);
    }

    let Some(variables) = parse_content_block_variables(ast, &mut errors) else {
        return (None, errors);
    };

    let children = transform(&ast.children);
    let node = t::ContentBlock {
        name,
        variables,
        children,
        spans: block_spans(ast, r3_span(&ast.source_span)),
        i18n: None,
    };
    (Some(node), errors)
}

/// Parses the `let` variables of a `@content` block (`parseContentBlockVariables`). `None` when the
/// variables do not start with `let`.
fn parse_content_block_variables(
    ast: &html::Block,
    errors: &mut Vec<ParseError>,
) -> Option<Vec<t::Variable>> {
    let mut variables: Vec<t::Variable> = Vec::new();
    if ast.parameters.len() < 2 {
        return Some(variables);
    }

    let vars_param = &ast.parameters[1];
    let vars_expr = vars_param.expression.trim();
    let Some((_, raw_names)) = match_for_loop_let(vars_expr) else {
        errors.push(ParseError::new(
            vars_param.source_span.clone(),
            "@content block variables must start with \"let\", e.g. \"let item, index\"",
        ));
        return None;
    };

    let variables_raw_string = raw_names;
    let variables_start_offset = vars_param
        .expression
        .find(&variables_raw_string)
        .unwrap_or(0) as i64;
    let variables_start_location = vars_param.source_span.start.move_by(variables_start_offset as isize);

    let mut search_index: usize = 0;
    for raw_name in variables_raw_string.split(',').map(|v| v.trim().to_string()) {
        let mut var_name = raw_name;
        if var_name.is_empty() {
            errors.push(ParseError::new(
                vars_param.source_span.clone(),
                "Invalid variable name in @content block",
            ));
            continue;
        }

        let index = variables_raw_string[search_index..]
            .find(&var_name)
            .map(|i| i + search_index)
            .unwrap_or(search_index);
        let var_start = variables_start_location.move_by(index as isize);

        let var_span: MlSpan;
        if let Some(eq_index) = var_name.find('=') {
            let name_part = var_name[..eq_index].trim().to_string();
            let full_var_span = MlSpan::new(
                var_start.clone(),
                var_start.move_by(var_name.chars().count() as isize),
            );
            errors.push(ParseError::new(
                full_var_span,
                "@content block variables cannot be assigned a value",
            ));
            var_name = name_part;
            var_span = MlSpan::new(
                var_start.clone(),
                var_start.move_by(var_name.chars().count() as isize),
            );
        } else {
            var_span = MlSpan::new(
                var_start.clone(),
                var_start.move_by(var_name.chars().count() as isize),
            );
        }

        if !is_identifier(&var_name) {
            errors.push(ParseError::new(
                var_span.clone(),
                format!("Variable name \"{var_name}\" must be a valid JavaScript identifier"),
            ));
            search_index = index + var_name.len();
            continue;
        }

        if variables.iter().any(|v| v.name == var_name) {
            errors.push(ParseError::new(
                var_span.clone(),
                format!("Duplicate variable name \"{var_name}\" in @content block"),
            ));
            search_index = index + var_name.len();
            continue;
        }

        // Content variables take no value: the render call passes them positionally, so the
        // `value` is empty (`new t.Variable(varName, '', varSpan, varSpan)`).
        let r3 = r3_span(&var_span);
        variables.push(t::Variable {
            name: var_name.clone(),
            value: String::new(),
            source_span: r3.clone(),
            key_span: r3,
            value_span: None,
        });
        search_index = index + var_name.len();
    }
    Some(variables)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml_parser::{parse as html_parse, Node as MlNode};

    fn content_block(src: &str) -> (Option<t::ContentBlock>, Vec<ParseError>) {
        let result = html_parse(src, "test.html");
        assert!(result.errors.is_empty(), "html parse errors: {:?}", result.errors);
        let block = result
            .root_nodes
            .into_iter()
            .find_map(|n| match n {
                MlNode::Block(b) => Some(*b),
                _ => None,
            })
            .expect("a block");
        let mut transform = |_nodes: &[html::Node]| Vec::new();
        create_content_block(&block, &mut transform)
    }

    #[test]
    fn parses_name_and_positional_variables() {
        let (node, errors) = content_block("@content(items; let item, index) {x}");
        assert!(errors.is_empty(), "{errors:?}");
        let node = node.expect("content node");
        assert_eq!(node.name, "items");
        let names: Vec<&str> = node.variables.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, vec!["item", "index"]);
        assert!(node.variables.iter().all(|v| v.value.is_empty()));
    }

    #[test]
    fn name_only_has_no_variables() {
        let (node, errors) = content_block("@content(header) {x}");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(node.unwrap().variables.is_empty());
    }

    #[test]
    fn rejects_bad_parameter_shapes() {
        let (node, errors) = content_block("@content {x}");
        assert!(node.is_none());
        assert!(errors[0].msg.starts_with("@content block must have one or two parameters"));

        let (node, errors) = content_block("@content(a, b) {x}");
        assert!(node.is_none());
        assert_eq!(errors[0].msg, "@content block must have exactly one name parameter");

        let (node, errors) = content_block("@content(1x) {x}");
        assert!(node.is_none());
        assert_eq!(errors[0].msg, "@content name must be a valid JavaScript identifier");

        let (node, errors) = content_block("@content(a; item) {x}");
        assert!(node.is_none());
        assert_eq!(
            errors[0].msg,
            "@content block variables must start with \"let\", e.g. \"let item, index\""
        );
    }

    #[test]
    fn reports_variable_errors_but_keeps_the_block() {
        let (node, errors) = content_block("@content(a; let x = 1, x, 2bad) {y}");
        let node = node.expect("content node kept");
        assert_eq!(node.variables.iter().map(|v| v.name.as_str()).collect::<Vec<_>>(), vec!["x"]);
        assert!(errors.iter().any(|e| e.msg == "@content block variables cannot be assigned a value"));
        assert!(errors.iter().any(|e| e.msg == "Duplicate variable name \"x\" in @content block"));
        assert!(errors
            .iter()
            .any(|e| e.msg == "Variable name \"2bad\" must be a valid JavaScript identifier"));
    }
}
