//! The HTML AST -> render3 (Ivy) template AST transform (`HtmlAstToIvyAst` /
//! `htmlAstToRender3Ast`) — the "front door" of template compilation.
//!
//! PORT TARGET: `migration/render3-specs/07-template_transform.md`
//! Source: `tools/angular-ref/packages/compiler/src/render3/r3_template_transform.ts`
//! (Angular 22.1.0-next.0).
//!
//! This module walks the [`crate::ml_parser`] HTML AST and produces a
//! [`crate::template::r3_ast`] template AST. It recognizes Angular-specific binding syntax
//! embedded in attribute names (`[prop]`, `(event)`, `[(banana)]`, `*structural`, `#ref`,
//! `let-x`, `@input`, `bind-`, `on-`, `bindon-`, `ref-`), splits attributes into
//! static / bound inputs / outputs / references / variables, handles selectorless
//! `Component`/`Directive` nodes, and delegates control-flow blocks to
//! [`crate::template::control_flow`] (`@if`/`@for`/`@switch`).
//!
//! ## Span model
//!
//! The project has two distinct `ParseSourceSpan` representations:
//!   - [`crate::ml_parser::ParseSourceSpan`] — location-based (`ParseLocation` w/ `offset`),
//!     carried by the *input* HTML nodes.
//!   - [`crate::expression::ast::ParseSourceSpan`] — offset-only (`{ start, end }`), carried by
//!     the *output* r3_ast nodes and the expression AST.
//! The transform bridges spans via [`to_offset_span`] wherever a span flows from an HTML node
//! into an r3_ast node. Note that `ParsedProperty`/`ParsedEvent`/`BoundElementProperty` (from
//! `expression::ast`) already carry offset spans, so those flow through unchanged.
//!
//! ## Sibling delegation / not-yet-ported
//!
//! - Control flow `@if`/`@for`/`@switch` are delegated to the real
//!   [`crate::template::control_flow`] `pub fn`s (`create_if_block`, `create_for_loop`,
//!   `create_switch_block`) via that module's [`crate::template::control_flow::ChildVisitor`]
//!   trait, threaded back into this visitor.
//! - `@defer` lives in [`crate::template::deferred`], which is still a stub at the time of
//!   writing. NOTE(port): swap [`create_deferred_block_local`] for
//!   `deferred::create_deferred_block` once that module lands. Until then `@defer` (and its
//!   connected `@placeholder`/`@loading`/`@error`) degrade to an [`t::UnknownBlock`].
//! - `BindingParser` — the spec delegates all expression parsing here. A local [`BindingParser`]
//!   wraps the ported [`crate::expression::parser::Parser`] and implements the subset the
//!   transform calls, accumulating `ParsedProperty`/`ParsedEvent`/`ParsedVariable` exactly like
//!   the TS `BindingParser`. NOTE(port): replace with `crate::template::binding_parser`.
//! - `preparseElement` / `isStyleUrlResolvable` / `isNgTemplate` / `replaceNgsp` are local
//!   helpers. NOTE(port): replace with `template_preparser` / `style_url_resolver` /
//!   `ml_parser::tags` / `ml_parser::html_whitespaces`.
//! - i18n meta is not yet modeled on the ml_parser nodes, so all i18n handling (root detection,
//!   ICU expansion) is inert (`None`), matching the TS "no i18n block" path.

use crate::expression::ast::{
    AstWithSource, BindingType, BoundElementProperty, ExprKind, ParseError as ExprParseError,
    ParseSourceSpan as OffsetSpan, ParsedEvent, ParsedEventType, ParsedProperty,
    ParsedPropertyType, ParsedVariable, SecurityContext,
};
use crate::expression::parser::Parser as ExprParser;
use crate::ml_parser as html;
use crate::ml_parser::{ParseError, ParseErrorLevel, ParseSourceSpan};
use crate::template::control_flow;
use crate::template::r3_ast as t;

use std::collections::HashSet;

// ===========================================================================
// Span bridging.
// ===========================================================================

/// Convert a location-based ml_parser span into the offset-only span used by r3_ast.
fn to_offset_span(span: &ParseSourceSpan) -> OffsetSpan {
    OffsetSpan {
        start: span.start.offset as u32,
        end: span.end.offset as u32,
    }
}

fn to_offset_span_opt(span: &Option<ParseSourceSpan>) -> Option<OffsetSpan> {
    span.as_ref().map(to_offset_span)
}

/// `span.start.offset` as the absolute offset the expression parser expects.
fn abs_offset(span: &ParseSourceSpan) -> i32 {
    span.start.offset as i32
}

// ===========================================================================
// Constants: BIND_NAME_REGEXP replacement, delimiters, tag sets.
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttrKind {
    Bind,
    Let,
    Ref,
    On,
    Bindon,
    At,
}

/// Equivalent of `name.match(BIND_NAME_REGEXP)`, allocation-free. Returns the matched prefix
/// kind, the literal prefix (needed by `createKeySpan`), and the trailing identifier.
/// Mirrors `^(?:(bind-)|(let-)|(ref-|#)|(on-)|(bindon-)|(@))(.*)$`.
fn classify_attr_prefix(name: &str) -> Option<(AttrKind, &'static str, &str)> {
    if let Some(rest) = name.strip_prefix("bind-") {
        return Some((AttrKind::Bind, "bind-", rest));
    }
    if let Some(rest) = name.strip_prefix("let-") {
        return Some((AttrKind::Let, "let-", rest));
    }
    if let Some(rest) = name.strip_prefix("ref-") {
        return Some((AttrKind::Ref, "ref-", rest));
    }
    if let Some(rest) = name.strip_prefix('#') {
        return Some((AttrKind::Ref, "#", rest));
    }
    if let Some(rest) = name.strip_prefix("on-") {
        return Some((AttrKind::On, "on-", rest));
    }
    if let Some(rest) = name.strip_prefix("bindon-") {
        return Some((AttrKind::Bindon, "bindon-", rest));
    }
    if let Some(rest) = name.strip_prefix('@') {
        return Some((AttrKind::At, "@", rest));
    }
    None
}

const BANANA_BOX: (&str, &str) = ("[(", ")]");
const PROPERTY: (&str, &str) = ("[", "]");
const EVENT: (&str, &str) = ("(", ")");
const TEMPLATE_ATTR_PREFIX: char = '*';

fn is_unsupported_selectorless_tag(name: &str) -> bool {
    matches!(
        name,
        "link" | "style" | "script" | "ng-template" | "ng-container" | "ng-content"
    )
}

fn is_unsupported_selectorless_directive_attr(name: &str) -> bool {
    matches!(name, "ngProjectAs" | "ngNonBindable")
}

/// `isConnectedDeferLoopBlock`. NOTE(port): use `deferred::is_connected_defer_loop_block` once
/// the `deferred` module is no longer a stub.
fn is_connected_defer_loop_block(name: &str) -> bool {
    matches!(name, "placeholder" | "loading" | "error")
}

// ===========================================================================
// Public result types.
// ===========================================================================

/// `Render3ParseResult` — the output of [`html_ast_to_render3_ast`].
#[derive(Clone, Debug, PartialEq)]
pub struct Render3ParseResult {
    pub nodes: Vec<t::Node>,
    pub errors: Vec<ParseError>,
    pub styles: Vec<String>,
    pub style_urls: Vec<String>,
    pub ng_content_selectors: Vec<String>,
    /// `Some` iff `options.collect_comment_nodes` was set.
    pub comment_nodes: Option<Vec<t::Comment>>,
}

/// `Render3ParseOptions`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Render3ParseOptions {
    pub collect_comment_nodes: bool,
}

/// `htmlAstToRender3Ast` — the sole public entry point.
pub fn html_ast_to_render3_ast(
    html_nodes: &[html::Node],
    binding_parser: &mut BindingParser,
    options: Render3ParseOptions,
) -> Render3ParseResult {
    // Angular default (`preserveWhitespaces = false`): strip insignificant whitespace text nodes
    // and collapse internal whitespace runs before lowering to the r3 AST. See the
    // `WhitespaceVisitor` invocation in `render3/view/template.ts` (run with
    // `preserveSignificantWhitespace = true`, `requireContext = false`).
    // NOTE(port): replace with `crate::ml_parser::html_whitespaces::remove_whitespaces`.
    let trimmed = remove_whitespaces(html_nodes);

    let mut transformer = HtmlAstToIvyAst::new(binding_parser, options);
    // `visitAll(this, htmlNodes, htmlNodes)` — context is the siblings array itself.
    let ivy_nodes = transformer.visit_all(&trimmed, &trimmed);

    // Errors come from two sources: the binding parser and the transformer.
    let mut all_errors = transformer.binding_parser.take_errors();
    all_errors.extend(transformer.errors.drain(..));

    let comment_nodes = if options.collect_comment_nodes {
        Some(std::mem::take(&mut transformer.comment_nodes))
    } else {
        None
    };

    Render3ParseResult {
        nodes: ivy_nodes,
        errors: all_errors,
        styles: std::mem::take(&mut transformer.styles),
        style_urls: std::mem::take(&mut transformer.style_urls),
        ng_content_selectors: std::mem::take(&mut transformer.ng_content_selectors),
        comment_nodes,
    }
}

// ===========================================================================
// prepareAttributes / categorizePropertyAttributes return records.
// ===========================================================================

/// `categorizePropertyAttributes` return record.
struct CategorizedAttrs {
    bound: Vec<t::BoundAttribute>,
    literal: Vec<t::TextAttribute>,
}

/// `prepareAttributes` return record.
#[derive(Default)]
struct PreparedAttributes {
    attributes: Vec<t::TextAttribute>,
    bound_events: Vec<t::BoundEvent>,
    references: Vec<t::Reference>,
    variables: Vec<t::Variable>,
    template_variables: Vec<t::Variable>,
    element_has_inline_template: bool,
    parsed_properties: Vec<ParsedProperty>,
    template_parsed_properties: Vec<ParsedProperty>,
}

// ===========================================================================
// HtmlAstToIvyAst — the main visitor.
// ===========================================================================

struct HtmlAstToIvyAst<'b> {
    binding_parser: &'b mut BindingParser,
    options: Render3ParseOptions,
    errors: Vec<ParseError>,
    styles: Vec<String>,
    style_urls: Vec<String>,
    ng_content_selectors: Vec<String>,
    comment_nodes: Vec<t::Comment>,
    in_i18n_block: bool,
    /// Identity set of already-consumed connected blocks / blank text nodes, keyed by source-span
    /// offsets (stable per node in a single parse). Mirrors the TS `processedNodes` identity set.
    processed_nodes: HashSet<(usize, usize)>,
}

/// Stable identity key for an HTML node (its source span offsets).
fn node_key(span: &ParseSourceSpan) -> (usize, usize) {
    (span.start.offset, span.end.offset)
}

impl<'b> HtmlAstToIvyAst<'b> {
    fn new(binding_parser: &'b mut BindingParser, options: Render3ParseOptions) -> Self {
        HtmlAstToIvyAst {
            binding_parser,
            options,
            errors: Vec::new(),
            styles: Vec::new(),
            style_urls: Vec::new(),
            ng_content_selectors: Vec::new(),
            comment_nodes: Vec::new(),
            in_i18n_block: false,
            processed_nodes: HashSet::new(),
        }
    }

    fn report_error(&mut self, message: impl Into<String>, span: &ParseSourceSpan) {
        self.errors.push(ParseError {
            span: span.clone(),
            msg: message.into(),
            level: ParseErrorLevel::Error,
            element_name: None,
        });
    }

    /// `html.visitAll(this, nodes, context)` — visit every node, dropping `None` results and
    /// flattening the (rare) multi-node results from `NonBindableVisitor`/`visitBlock`.
    fn visit_all(&mut self, nodes: &[html::Node], context: &[html::Node]) -> Vec<t::Node> {
        let mut out = Vec::new();
        for node in nodes {
            self.visit_node_into(node, context, &mut out);
        }
        out
    }

    fn visit_node_into(
        &mut self,
        node: &html::Node,
        context: &[html::Node],
        out: &mut Vec<t::Node>,
    ) {
        match node {
            html::Node::Element(e) => {
                if let Some(n) = self.visit_element(e) {
                    out.push(n);
                }
            }
            html::Node::Text(text) => {
                if let Some(n) = self.visit_text(text) {
                    out.push(n);
                }
            }
            html::Node::Comment(c) => self.visit_comment(c),
            html::Node::Expansion(e) => {
                if let Some(icu) = self.visit_expansion(e) {
                    out.push(t::Node::Icu(icu));
                }
            }
            html::Node::ExpansionCase(_) => { /* folded into the parent expansion */ }
            html::Node::Block(b) => {
                if let Some(n) = self.visit_block(b, context) {
                    out.push(n);
                }
            }
            html::Node::BlockParameter(_) => {}
            html::Node::Component(c) => {
                if let Some(n) = self.visit_component(c) {
                    out.push(n);
                }
            }
            html::Node::Directive(_) => {}
            html::Node::Attribute(_) => {}
            html::Node::LetDeclaration(d) => out.push(self.visit_let_declaration(d)),
        }
    }

    // ----- visitElement -----------------------------------------------------

    fn visit_element(&mut self, element: &html::Element) -> Option<t::Node> {
        // NOTE(port): i18n root detection needs the i18n module; inert here.
        let is_i18n_root_element = false;
        let _ = self.in_i18n_block;

        let preparsed = preparse_element(&element.name, &element.attrs, &element.children);
        match preparsed.kind {
            PreparsedElementType::Script => return None,
            PreparsedElementType::Style => {
                if let Some(contents) = text_contents(&element.children) {
                    self.styles.push(contents);
                }
                return None;
            }
            PreparsedElementType::Stylesheet if is_style_url_resolvable(&preparsed.href_attr) => {
                self.style_urls.push(preparsed.href_attr.clone());
                return None;
            }
            _ => {}
        }

        let is_template_element = is_ng_template(&element.name);
        let prepared = self.prepare_attributes(&element.attrs, is_template_element);
        let directives = self.extract_directives(Some(&element.name), &element.directives);

        let children = if preparsed.non_bindable {
            NonBindableVisitor.visit_all(&element.children)
        } else {
            self.visit_all(&element.children, &element.children)
        };

        let mut parsed_element: t::Node = if preparsed.kind == PreparsedElementType::NgContent {
            let selector = preparsed.select_attr.clone();
            let attrs: Vec<t::TextAttribute> = element.attrs.iter().map(visit_attribute).collect();
            self.ng_content_selectors.push(selector.clone());
            t::Node::Content(t::Content {
                selector,
                attributes: attrs,
                children,
                is_self_closing: element.is_self_closing,
                source_span: to_offset_span(&element.source_span),
                start_source_span: to_offset_span(&element.start_source_span),
                end_source_span: to_offset_span_opt(&element.end_source_span),
                i18n: None,
            })
        } else if is_template_element {
            let attrs = self
                .categorize_property_attributes(Some(&element.name), &prepared.parsed_properties);
            t::Node::Template(t::Template {
                tag_name: Some(element.name.clone()),
                attributes: prepared.attributes.clone(),
                inputs: attrs.bound,
                outputs: prepared.bound_events.clone(),
                directives,
                template_attrs: Vec::new(),
                children,
                references: prepared.references.clone(),
                variables: prepared.variables.clone(),
                is_self_closing: element.is_self_closing,
                source_span: to_offset_span(&element.source_span),
                start_source_span: to_offset_span(&element.start_source_span),
                end_source_span: to_offset_span_opt(&element.end_source_span),
                i18n: None,
            })
        } else {
            let attrs = self
                .categorize_property_attributes(Some(&element.name), &prepared.parsed_properties);
            if element.name == "ng-container" {
                for bound in &attrs.bound {
                    if bound.kind == BindingType::Attribute {
                        let span = element.source_span.clone();
                        self.report_error(
                            "Attribute bindings are not supported on ng-container. Use property bindings instead.",
                            &span,
                        );
                    }
                }
            }
            t::Node::Element(t::Element {
                name: element.name.clone(),
                attributes: prepared.attributes.clone(),
                inputs: attrs.bound,
                outputs: prepared.bound_events.clone(),
                directives,
                children,
                references: prepared.references.clone(),
                is_self_closing: element.is_self_closing,
                source_span: to_offset_span(&element.source_span),
                start_source_span: to_offset_span(&element.start_source_span),
                end_source_span: to_offset_span_opt(&element.end_source_span),
                is_void: element.is_void,
                i18n: None,
            })
        };

        if prepared.element_has_inline_template {
            parsed_element = self.wrap_in_template(
                parsed_element,
                &prepared.template_parsed_properties,
                prepared.template_variables.clone(),
                is_template_element,
                is_i18n_root_element,
                &element.source_span,
                &element.start_source_span,
                &element.end_source_span,
            );
        }

        Some(parsed_element)
    }

    // ----- visitComponent ---------------------------------------------------

    fn visit_component(&mut self, component: &html::Component) -> Option<t::Node> {
        let is_i18n_root_element = false; // NOTE(port): i18n not modeled.

        if let Some(tag) = &component.tag_name {
            if is_unsupported_selectorless_tag(tag) {
                let span = component.start_source_span.clone();
                self.report_error(
                    format!("Tag name \"{tag}\" cannot be used as a component tag"),
                    &span,
                );
                return None;
            }
        }

        let prepared = self.prepare_attributes(&component.attrs, false);
        self.validate_selectorless_references(&prepared.references);
        let directives =
            self.extract_directives(component.tag_name.as_deref(), &component.directives);

        let children = if component.attrs.iter().any(|a| a.name == "ngNonBindable") {
            NonBindableVisitor.visit_all(&component.children)
        } else {
            self.visit_all(&component.children, &component.children)
        };

        let attrs = self.categorize_property_attributes(
            component.tag_name.as_deref(),
            &prepared.parsed_properties,
        );

        let mut node: t::Node = t::Node::Component(t::Component {
            component_name: component.component_name.clone(),
            tag_name: component.tag_name.clone(),
            full_name: component.full_name.clone(),
            attributes: prepared.attributes.clone(),
            inputs: attrs.bound,
            outputs: prepared.bound_events.clone(),
            directives,
            children,
            references: prepared.references.clone(),
            is_self_closing: component.is_self_closing,
            source_span: to_offset_span(&component.source_span),
            start_source_span: to_offset_span(&component.start_source_span),
            end_source_span: to_offset_span_opt(&component.end_source_span),
            i18n: None,
        });

        if prepared.element_has_inline_template {
            node = self.wrap_in_template(
                node,
                &prepared.template_parsed_properties,
                prepared.template_variables.clone(),
                false,
                is_i18n_root_element,
                &component.source_span,
                &component.start_source_span,
                &component.end_source_span,
            );
        }

        Some(node)
    }

    // ----- visitText --------------------------------------------------------

    fn visit_text(&mut self, text: &html::Text) -> Option<t::Node> {
        if self.processed_nodes.contains(&node_key(&text.source_span)) {
            return None;
        }
        Some(self.visit_text_with_interpolation(&text.value, &text.source_span))
    }

    fn visit_text_with_interpolation(&mut self, value: &str, span: &ParseSourceSpan) -> t::Node {
        let value_no_ngsp = replace_ngsp(value);
        match self.binding_parser.parse_interpolation(&value_no_ngsp, span) {
            Some(ast) => t::Node::BoundText(t::BoundText {
                value: *ast.ast,
                source_span: to_offset_span(span),
                i18n: None,
            }),
            None => t::Node::Text(t::Text {
                value: value_no_ngsp,
                source_span: to_offset_span(span),
            }),
        }
    }

    // ----- visitExpansion (ICU) ---------------------------------------------

    fn visit_expansion(&mut self, _expansion: &html::Expansion) -> Option<t::Icu> {
        // NOTE(port): ICUs are only meaningful inside an i18n block, and `expansion.i18n` is not
        // modeled on the ml_parser node yet. The TS code returns `null` when `expansion.i18n` is
        // absent, so this mirrors that path (always `None`).
        None
    }

    // ----- visitComment -----------------------------------------------------

    fn visit_comment(&mut self, comment: &html::Comment) {
        if self.options.collect_comment_nodes {
            self.comment_nodes.push(t::Comment {
                value: comment.value.clone().unwrap_or_default(),
                source_span: to_offset_span(&comment.source_span),
            });
        }
    }

    // ----- visitLetDeclaration ----------------------------------------------

    fn visit_let_declaration(&mut self, decl: &html::LetDeclaration) -> t::Node {
        let value = self
            .binding_parser
            .parse_binding(&decl.value, false, &decl.value_span);
        if value.errors.is_empty() && matches!(value.ast.kind, ExprKind::EmptyExpr) {
            let span = decl.value_span.clone();
            self.report_error("@let declaration value cannot be empty", &span);
        }
        t::Node::LetDeclaration(t::LetDeclaration {
            name: decl.name.clone(),
            value: *value.ast,
            source_span: to_offset_span(&decl.source_span),
            name_span: to_offset_span(&decl.name_span),
            value_span: to_offset_span(&decl.value_span),
        })
    }

    // ----- visitBlock (control flow) ----------------------------------------

    fn visit_block(&mut self, block: &html::Block, context: &[html::Node]) -> Option<t::Node> {
        let index = context.iter().position(|n| match n {
            html::Node::Block(b) => node_key(&b.source_span) == node_key(&block.source_span),
            _ => false,
        });
        let index = match index {
            Some(i) => i,
            None => {
                let span = block.source_span.clone();
                self.report_error(
                    "Visitor invoked incorrectly. Expecting visitBlock to be invoked with the siblings array as its context",
                    &span,
                );
                return None;
            }
        };

        if self.processed_nodes.contains(&node_key(&block.source_span)) {
            return None;
        }

        // The control_flow `create_*` functions are stateless w.r.t. the expression parser.
        let parser = ExprParser::default();

        let (node, errors): (Option<t::Node>, Vec<ParseError>) = match block.name.as_str() {
            "defer" => {
                let connected =
                    self.find_connected_blocks(index, context, is_connected_defer_loop_block);
                // NOTE(port): delegate to `deferred::create_deferred_block` once ported.
                create_deferred_block_local(block, &connected)
            }
            "switch" => {
                let (n, e) = control_flow::create_switch_block(block, self, &parser);
                (n.map(t::Node::SwitchBlock), e)
            }
            "for" => {
                let connected = self.find_connected_blocks(
                    index,
                    context,
                    control_flow::is_connected_for_loop_block,
                );
                let (n, e) = control_flow::create_for_loop(block, &connected, self, &parser);
                (n.map(t::Node::ForLoopBlock), e)
            }
            "if" => {
                let connected =
                    self.find_connected_blocks(index, context, control_flow::is_connected_if_block);
                let (n, e) = control_flow::create_if_block(block, &connected, self, &parser);
                (n.map(t::Node::IfBlock), e)
            }
            other => {
                let (error_message, mark_processed) = if is_connected_defer_loop_block(other) {
                    (
                        format!("@{other} block can only be used after an @defer block."),
                        true,
                    )
                } else if control_flow::is_connected_for_loop_block(other) {
                    (
                        format!("@{other} block can only be used after an @for block."),
                        true,
                    )
                } else if control_flow::is_connected_if_block(other) {
                    (
                        format!("@{other} block can only be used after an @if or @else if block."),
                        true,
                    )
                } else {
                    (format!("Unrecognized block @{other}."), false)
                };
                if mark_processed {
                    self.processed_nodes.insert(node_key(&block.source_span));
                }
                (
                    Some(t::Node::UnknownBlock(t::UnknownBlock {
                        name: block.name.clone(),
                        source_span: to_offset_span(&block.source_span),
                        name_span: to_offset_span(&block.name_span),
                    })),
                    vec![ParseError {
                        span: block.source_span.clone(),
                        msg: error_message,
                        level: ParseErrorLevel::Error,
                        element_name: None,
                    }],
                )
            }
        };

        self.errors.extend(errors);
        node
    }

    /// `findConnectedBlocks` — scan forward from `primary_index + 1`, skipping comments and blank
    /// text (marked processed), collecting related blocks (also marked processed), stopping at the
    /// first non-block or unrelated block.
    fn find_connected_blocks(
        &mut self,
        primary_index: usize,
        siblings: &[html::Node],
        predicate: fn(&str) -> bool,
    ) -> Vec<html::Block> {
        let mut related = Vec::new();
        let mut i = primary_index + 1;
        while i < siblings.len() {
            match &siblings[i] {
                html::Node::Comment(_) => {}
                html::Node::Text(text) if text.value.trim().is_empty() => {
                    self.processed_nodes.insert(node_key(&text.source_span));
                }
                html::Node::Block(b) if predicate(&b.name) => {
                    related.push((**b).clone());
                    self.processed_nodes.insert(node_key(&b.source_span));
                }
                _ => break,
            }
            i += 1;
        }
        related
    }

    // ----- attribute categorization -----------------------------------------

    fn categorize_property_attributes(
        &mut self,
        element_name: Option<&str>,
        properties: &[ParsedProperty],
    ) -> CategorizedAttrs {
        let mut bound = Vec::new();
        let mut literal = Vec::new();

        for prop in properties {
            if prop.is_literal() {
                literal.push(t::TextAttribute {
                    name: prop.name.clone(),
                    value: prop.expression.source.clone().unwrap_or_default(),
                    // ParsedProperty spans are already offset-based.
                    source_span: prop.source_span.clone(),
                    key_span: Some(prop.key_span.clone()),
                    value_span: prop.value_span.clone(),
                    i18n: None,
                });
            } else {
                let is_attr_on = prop.name.to_lowercase().starts_with("attr.on");
                let bep = self.binding_parser.create_bound_element_property(
                    element_name,
                    prop,
                    !is_attr_on,
                    false,
                );
                bound.push(t::BoundAttribute::from_bound_element_property(bep, None));
            }
        }

        CategorizedAttrs { bound, literal }
    }

    fn prepare_attributes(
        &mut self,
        attrs: &[html::Attribute],
        is_template_element: bool,
    ) -> PreparedAttributes {
        let mut prepared = PreparedAttributes::default();

        for attribute in attrs {
            let mut has_binding = false;
            let mut is_template_binding = false;

            if attribute.name.starts_with(TEMPLATE_ATTR_PREFIX) {
                if prepared.element_has_inline_template {
                    let span = attribute.source_span.clone();
                    self.report_error(
                        "Can't have multiple template bindings on one element. Use only one attribute prefixed with *",
                        &span,
                    );
                }
                is_template_binding = true;
                prepared.element_has_inline_template = true;
                let template_value = &attribute.value;
                let template_key = &attribute.name[TEMPLATE_ATTR_PREFIX.len_utf8()..];

                // absoluteValueOffset: valueSpan.fullStart, else sourceSpan.fullStart + name.len.
                let absolute_value_offset = match &attribute.value_span {
                    Some(vs) => vs.full_start.offset as i32,
                    None => (attribute.source_span.full_start.offset
                        + attribute.name.chars().count()) as i32,
                };

                let mut parsed_variables: Vec<ParsedVariable> = Vec::new();
                self.binding_parser.parse_inline_template_binding(
                    template_key,
                    template_value,
                    &attribute.source_span,
                    absolute_value_offset,
                    &mut prepared.template_parsed_properties,
                    &mut parsed_variables,
                );
                for v in parsed_variables {
                    prepared.template_variables.push(t::Variable {
                        name: v.name,
                        value: v.value,
                        source_span: v.source_span,
                        key_span: v.key_span,
                        value_span: v.value_span,
                    });
                }
            } else {
                has_binding = self.parse_attribute(
                    is_template_element,
                    attribute,
                    &mut prepared.parsed_properties,
                    &mut prepared.bound_events,
                    &mut prepared.variables,
                    &mut prepared.references,
                );
            }

            if !has_binding && !is_template_binding {
                prepared.attributes.push(visit_attribute(attribute));
            }
        }

        prepared
    }

    /// `parseAttribute` — the binding classifier. Returns whether a binding was found.
    fn parse_attribute(
        &mut self,
        is_template_element: bool,
        attribute: &html::Attribute,
        parsed_properties: &mut Vec<ParsedProperty>,
        bound_events: &mut Vec<t::BoundEvent>,
        variables: &mut Vec<t::Variable>,
        references: &mut Vec<t::Reference>,
    ) -> bool {
        let name = &attribute.name;
        let value = &attribute.value;
        let src_span = &attribute.source_span;
        let absolute_offset = match &attribute.value_span {
            Some(vs) => vs.full_start.offset as i32,
            None => src_span.full_start.offset as i32,
        };

        if let Some((kind, prefix, identifier)) = classify_attr_prefix(name) {
            match kind {
                AttrKind::Bind => {
                    let key_span = create_key_span(src_span, prefix, identifier);
                    self.binding_parser.parse_property_binding(
                        identifier,
                        value,
                        false,
                        src_span,
                        absolute_offset,
                        &attribute.value_span,
                        parsed_properties,
                        &key_span,
                    );
                }
                AttrKind::Let => {
                    if is_template_element {
                        let key_span = create_key_span(src_span, prefix, identifier);
                        self.parse_variable(
                            identifier,
                            value,
                            src_span,
                            &key_span,
                            &attribute.value_span,
                            variables,
                        );
                    } else {
                        self.report_error(
                            "\"let-\" is only supported on ng-template elements.",
                            src_span,
                        );
                    }
                }
                AttrKind::Ref => {
                    let key_span = create_key_span(src_span, prefix, identifier);
                    self.parse_reference(
                        identifier,
                        value,
                        src_span,
                        &key_span,
                        &attribute.value_span,
                        references,
                    );
                }
                AttrKind::On => {
                    let key_span = create_key_span(src_span, prefix, identifier);
                    let mut events = Vec::new();
                    self.binding_parser.parse_event(
                        identifier,
                        value,
                        false,
                        src_span,
                        attribute.value_span.as_ref().unwrap_or(src_span),
                        &mut events,
                        &key_span,
                    );
                    add_events(events, bound_events);
                }
                AttrKind::Bindon => {
                    let key_span = create_key_span(src_span, prefix, identifier);
                    self.binding_parser.parse_property_binding(
                        identifier,
                        value,
                        true,
                        src_span,
                        absolute_offset,
                        &attribute.value_span,
                        parsed_properties,
                        &key_span,
                    );
                    self.parse_assignment_event(
                        identifier,
                        value,
                        src_span,
                        &attribute.value_span,
                        bound_events,
                        &key_span,
                    );
                }
                AttrKind::At => {
                    let key_span = create_key_span(src_span, "", name);
                    self.binding_parser.parse_literal_attr(
                        name,
                        value,
                        src_span,
                        absolute_offset,
                        &attribute.value_span,
                        parsed_properties,
                        &key_span,
                    );
                }
            }
            return true;
        }

        // []/()/[()] delimiter syntax.
        let delims: Option<(&str, &str)> = if name.starts_with(BANANA_BOX.0) {
            Some(BANANA_BOX)
        } else if name.starts_with(PROPERTY.0) {
            Some(PROPERTY)
        } else if name.starts_with(EVENT.0) {
            Some(EVENT)
        } else {
            None
        };

        if let Some((start, end)) = delims {
            // Legacy rule (do NOT "fix"): must end with the close delim and be longer than
            // start.len + end.len.
            if name.ends_with(end) && name.len() > start.len() + end.len() {
                let identifier = &name[start.len()..name.len() - end.len()];
                let key_span = create_key_span(src_span, start, identifier);
                if start == BANANA_BOX.0 {
                    self.binding_parser.parse_property_binding(
                        identifier,
                        value,
                        true,
                        src_span,
                        absolute_offset,
                        &attribute.value_span,
                        parsed_properties,
                        &key_span,
                    );
                    self.parse_assignment_event(
                        identifier,
                        value,
                        src_span,
                        &attribute.value_span,
                        bound_events,
                        &key_span,
                    );
                } else if start == PROPERTY.0 {
                    self.binding_parser.parse_property_binding(
                        identifier,
                        value,
                        false,
                        src_span,
                        absolute_offset,
                        &attribute.value_span,
                        parsed_properties,
                        &key_span,
                    );
                } else {
                    let mut events = Vec::new();
                    self.binding_parser.parse_event(
                        identifier,
                        value,
                        false,
                        src_span,
                        attribute.value_span.as_ref().unwrap_or(src_span),
                        &mut events,
                        &key_span,
                    );
                    add_events(events, bound_events);
                }
                return true;
            }
        }

        // No explicit binding: an attribute whose value may contain `{{ }}`.
        let key_span = create_key_span(src_span, "", name);
        self.binding_parser.parse_property_interpolation(
            name,
            value,
            src_span,
            &attribute.value_span,
            parsed_properties,
            &key_span,
        )
    }

    // ----- directives -------------------------------------------------------

    fn extract_directives(
        &mut self,
        element_name: Option<&str>,
        node_directives: &[html::Directive],
    ) -> Vec<t::Directive> {
        let mut directives = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for directive in node_directives {
            let mut invalid = false;

            for attr in &directive.attrs {
                if attr.name.starts_with(TEMPLATE_ATTR_PREFIX) {
                    invalid = true;
                    let span = attr.source_span.clone();
                    self.report_error(
                        format!(
                            "Shorthand template syntax \"{}\" is not supported inside a directive context",
                            attr.name
                        ),
                        &span,
                    );
                } else if is_unsupported_selectorless_directive_attr(&attr.name) {
                    invalid = true;
                    let span = attr.source_span.clone();
                    self.report_error(
                        format!(
                            "Attribute \"{}\" is not supported in a directive context",
                            attr.name
                        ),
                        &span,
                    );
                }
            }

            if !invalid && seen.contains(&directive.name) {
                invalid = true;
                let span = directive.source_span.clone();
                self.report_error(
                    format!(
                        "Cannot apply directive \"{}\" multiple times on the same element",
                        directive.name
                    ),
                    &span,
                );
            }

            if invalid {
                continue;
            }

            let prepared = self.prepare_attributes(&directive.attrs, false);
            self.validate_selectorless_references(&prepared.references);
            let categorized =
                self.categorize_property_attributes(element_name, &prepared.parsed_properties);
            let inputs = categorized.bound;

            for input in &inputs {
                if input.kind != BindingType::Property && input.kind != BindingType::TwoWay {
                    invalid = true;
                    let span = directive.source_span.clone();
                    self.report_error("Binding is not supported in a directive context", &span);
                }
            }

            if invalid {
                continue;
            }

            seen.insert(directive.name.clone());
            directives.push(t::Directive {
                name: directive.name.clone(),
                attributes: prepared.attributes,
                inputs,
                outputs: prepared.bound_events,
                references: prepared.references,
                source_span: to_offset_span(&directive.source_span),
                start_source_span: to_offset_span(&directive.start_source_span),
                end_source_span: to_offset_span_opt(&directive.end_source_span),
                i18n: None,
            });
        }

        directives
    }

    // ----- wrapInTemplate ---------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn wrap_in_template(
        &mut self,
        node: t::Node,
        template_properties: &[ParsedProperty],
        template_variables: Vec<t::Variable>,
        is_template_element: bool,
        is_i18n_root_element: bool,
        source_span: &ParseSourceSpan,
        start_source_span: &ParseSourceSpan,
        end_source_span: &Option<ParseSourceSpan>,
    ) -> t::Node {
        let attrs = self.categorize_property_attributes(Some("ng-template"), template_properties);
        let mut template_attrs: Vec<t::TemplateAttr> = Vec::new();
        for attr in attrs.literal {
            template_attrs.push(t::TemplateAttr::Text(attr));
        }
        for attr in attrs.bound {
            template_attrs.push(t::TemplateAttr::Bound(attr));
        }

        let mut hoisted_attributes: Vec<t::TextAttribute> = Vec::new();
        let mut hoisted_inputs: Vec<t::BoundAttribute> = Vec::new();
        let mut hoisted_outputs: Vec<t::BoundEvent> = Vec::new();

        match &node {
            t::Node::Element(e) => {
                hoisted_attributes.extend(filter_animation_attributes(&e.attributes));
                hoisted_inputs.extend(filter_animation_inputs(&e.inputs));
                hoisted_outputs.extend(e.outputs.iter().cloned());
            }
            t::Node::Component(c) => {
                hoisted_attributes.extend(filter_animation_attributes(&c.attributes));
                hoisted_inputs.extend(filter_animation_inputs(&c.inputs));
                hoisted_outputs.extend(c.outputs.iter().cloned());
            }
            _ => {}
        }

        // i18n: dropped for ng-template + i18n root (avoid duplicate i18n instructions).
        // NOTE(port): node.i18n is always None until i18n is modeled.
        let _ = (is_template_element, is_i18n_root_element);

        let name: Option<String> = match &node {
            t::Node::Component(c) => c.tag_name.clone(),
            t::Node::Template(_) => None,
            t::Node::Element(e) => Some(e.name.clone()),
            t::Node::Content(_) => Some(t::Content::NAME.to_string()),
            _ => None,
        };

        t::Node::Template(t::Template {
            tag_name: name,
            attributes: hoisted_attributes,
            inputs: hoisted_inputs,
            outputs: hoisted_outputs,
            directives: Vec::new(),
            template_attrs,
            children: vec![node],
            references: Vec::new(),
            variables: template_variables,
            is_self_closing: false,
            source_span: to_offset_span(source_span),
            start_source_span: to_offset_span(start_source_span),
            end_source_span: to_offset_span_opt(end_source_span),
            i18n: None,
        })
    }

    // ----- variables / references / assignment events -----------------------

    fn parse_variable(
        &mut self,
        identifier: &str,
        value: &str,
        source_span: &ParseSourceSpan,
        key_span: &ParseSourceSpan,
        value_span: &Option<ParseSourceSpan>,
        variables: &mut Vec<t::Variable>,
    ) {
        if identifier.contains('-') {
            self.report_error("\"-\" is not allowed in variable names", source_span);
        } else if identifier.is_empty() {
            self.report_error("Variable does not have a name", source_span);
        }
        variables.push(t::Variable {
            name: identifier.to_string(),
            value: value.to_string(),
            source_span: to_offset_span(source_span),
            key_span: to_offset_span(key_span),
            value_span: to_offset_span_opt(value_span),
        });
    }

    fn parse_reference(
        &mut self,
        identifier: &str,
        value: &str,
        source_span: &ParseSourceSpan,
        key_span: &ParseSourceSpan,
        value_span: &Option<ParseSourceSpan>,
        references: &mut Vec<t::Reference>,
    ) {
        if identifier.contains('-') {
            self.report_error("\"-\" is not allowed in reference names", source_span);
        } else if identifier.is_empty() {
            self.report_error("Reference does not have a name", source_span);
        } else if references.iter().any(|r| r.name == identifier) {
            self.report_error(
                format!("Reference \"#{identifier}\" is defined more than once"),
                source_span,
            );
        }
        references.push(t::Reference {
            name: identifier.to_string(),
            value: value.to_string(),
            source_span: to_offset_span(source_span),
            key_span: to_offset_span(key_span),
            value_span: to_offset_span_opt(value_span),
        });
    }

    fn parse_assignment_event(
        &mut self,
        name: &str,
        expression: &str,
        source_span: &ParseSourceSpan,
        value_span: &Option<ParseSourceSpan>,
        bound_events: &mut Vec<t::BoundEvent>,
        key_span: &ParseSourceSpan,
    ) {
        let mut events = Vec::new();
        self.binding_parser.parse_event(
            &format!("{name}Change"),
            expression,
            true,
            source_span,
            value_span.as_ref().unwrap_or(source_span),
            &mut events,
            key_span,
        );
        add_events(events, bound_events);
    }

    fn validate_selectorless_references(&mut self, references: &[t::Reference]) {
        if references.is_empty() {
            return;
        }
        let mut seen: HashSet<String> = HashSet::new();
        for r in references {
            if !r.value.is_empty() {
                self.errors.push(offset_error(
                    "Cannot specify a value for a local reference in this context",
                    r.value_span.clone().unwrap_or_else(|| r.source_span.clone()),
                ));
            } else if seen.contains(&r.name) {
                self.errors.push(offset_error(
                    "Duplicate reference names are not allowed",
                    r.source_span.clone(),
                ));
            } else {
                seen.insert(r.name.clone());
            }
        }
    }
}

/// `ChildVisitor` lets the `control_flow` block constructors re-enter this visitor for child
/// nodes (`html.visitAll(this, children, children)`).
impl control_flow::ChildVisitor for HtmlAstToIvyAst<'_> {
    fn visit_children(&mut self, children: &[html::Node]) -> Vec<t::Node> {
        self.visit_all(children, children)
    }
}

// ===========================================================================
// Helper free functions.
// ===========================================================================

/// `visitAttribute` — build a static `TextAttribute` from an HTML attribute.
fn visit_attribute(attribute: &html::Attribute) -> t::TextAttribute {
    t::TextAttribute {
        name: attribute.name.clone(),
        value: attribute.value.clone(),
        source_span: to_offset_span(&attribute.source_span),
        key_span: to_offset_span_opt(&attribute.key_span),
        value_span: to_offset_span_opt(&attribute.value_span),
        i18n: None,
    }
}

/// `addEvents` — convert `ParsedEvent`s into `BoundEvent`s and append them.
fn add_events(events: Vec<ParsedEvent>, bound_events: &mut Vec<t::BoundEvent>) {
    for e in events {
        bound_events.push(t::BoundEvent::from_parsed_event(e));
    }
}

/// `textContents` — content of an element with exactly one text child, else `None`.
fn text_contents(children: &[html::Node]) -> Option<String> {
    if children.len() != 1 {
        return None;
    }
    match &children[0] {
        html::Node::Text(t) => Some(t.value.clone()),
        _ => None,
    }
}

/// `createKeySpan(srcSpan, prefix, identifier)` — advance past the prefix to compute the key span.
fn create_key_span(src_span: &ParseSourceSpan, prefix: &str, identifier: &str) -> ParseSourceSpan {
    let start = src_span.start.move_by(prefix.chars().count() as isize);
    let end = start.move_by(identifier.chars().count() as isize);
    let full_start = start.clone();
    ParseSourceSpan::with_full_start(start, end, full_start, Some(identifier.to_string()))
}

fn filter_animation_attributes(attributes: &[t::TextAttribute]) -> Vec<t::TextAttribute> {
    attributes
        .iter()
        .filter(|a| !a.name.starts_with("animate."))
        .cloned()
        .collect()
}

fn filter_animation_inputs(inputs: &[t::BoundAttribute]) -> Vec<t::BoundAttribute> {
    inputs
        .iter()
        .filter(|a| a.kind != BindingType::Animation)
        .cloned()
        .collect()
}

/// `isNgTemplate`. NOTE(port): replace with `crate::ml_parser::tags::is_ng_template`.
fn is_ng_template(name: &str) -> bool {
    let stripped = name.rsplit(':').next().unwrap_or(name);
    stripped == "ng-template"
}

/// `replaceNgsp`. NOTE(port): replace with `crate::ml_parser::html_whitespaces::replace_ngsp`.
fn replace_ngsp(value: &str) -> String {
    // The NGSP marker is U+E500.
    value.replace('\u{E500}', " ")
}

// ===========================================================================
// WhitespaceVisitor / removeWhitespaces — local port.
// NOTE(port): replace with `crate::ml_parser::html_whitespaces`.
// Source: `tools/angular-ref/packages/compiler/src/ml_parser/html_whitespaces.ts`.
// ===========================================================================

/// Tags whose contents preserve whitespace verbatim (no trimming/collapsing, no descent).
/// `SKIP_WS_TRIM_TAGS` from `html_whitespaces.ts`.
const SKIP_WS_TRIM_TAGS: [&str; 5] = ["pre", "template", "textarea", "script", "style"];

/// Marker attribute that opts a subtree out of whitespace trimming.
const PRESERVE_WS_ATTR_NAME: &str = "ngPreserveWhitespaces";

/// `\s` with ` ` (non-breaking space) excluded — the `WS_CHARS` set from `html_whitespaces.ts`.
fn is_ws_char(c: char) -> bool {
    matches!(
        c,
        ' ' | '\u{0C}' // \f
            | '\n'
            | '\r'
            | '\t'
            | '\u{0B}' // \v
            | '\u{1680}'
            | '\u{180E}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `NO_WS_REGEXP.test(value)` — true iff the string contains at least one non-whitespace char.
fn has_non_ws(value: &str) -> bool {
    value.chars().any(|c| !is_ws_char(c))
}

/// `processWhitespace` — `replaceNgsp(text).replace(WS_REPLACE_REGEXP, ' ')`: convert the &ngsp;
/// marker to a space, then collapse runs of 2+ whitespace characters to a single space. Runs of
/// length 1 are left untouched (the regex only matches `{2,}`).
fn process_whitespace(value: &str) -> String {
    let replaced = replace_ngsp(value);
    let chars: Vec<char> = replaced.chars().collect();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < chars.len() {
        if is_ws_char(chars[i]) {
            let start = i;
            while i < chars.len() && is_ws_char(chars[i]) {
                i += 1;
            }
            if i - start >= 2 {
                out.push(' ');
            } else {
                out.push(chars[start]);
            }
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

/// `hasPreserveWhitespacesAttr`.
fn has_preserve_ws_attr(attrs: &[html::Attribute]) -> bool {
    attrs.iter().any(|a| a.name == PRESERVE_WS_ATTR_NAME)
}

/// `removeWhitespaces` / `WhitespaceVisitor` applied to a sibling list.
///
/// This mirrors how the render3 template parser drives the visitor when `preserveWhitespaces` is
/// false: `preserveSignificantWhitespace = true` and `requireContext = false`. Under those flags
/// the visitor never trims leading/trailing whitespace of a non-blank text node (no sibling
/// context), but it still:
///   - drops text nodes that are entirely whitespace,
///   - collapses internal whitespace runs to a single space,
///   - converts the &ngsp; marker to a space,
///   - and recurses into element/component/block/expansion-case children (except inside
///     whitespace-preserving tags or subtrees marked `ngPreserveWhitespaces`).
fn remove_whitespaces(nodes: &[html::Node]) -> Vec<html::Node> {
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        match node {
            html::Node::Text(text) => {
                if has_non_ws(&text.value) {
                    let mut new_text = (**text).clone();
                    new_text.value = process_whitespace(&text.value);
                    out.push(html::Node::Text(Box::new(new_text)));
                }
                // else: whitespace-only text node is dropped.
            }
            html::Node::Element(e) => {
                let mut new_e = (**e).clone();
                if !(SKIP_WS_TRIM_TAGS.contains(&e.name.as_str())
                    || has_preserve_ws_attr(&e.attrs))
                {
                    new_e.children = remove_whitespaces(&e.children);
                }
                // Drop the `ngPreserveWhitespaces` marker attribute (visitAttribute returns null).
                new_e
                    .attrs
                    .retain(|a| a.name != PRESERVE_WS_ATTR_NAME);
                out.push(html::Node::Element(Box::new(new_e)));
            }
            html::Node::Component(c) => {
                let mut new_c = (**c).clone();
                let skip = c
                    .tag_name
                    .as_deref()
                    .is_some_and(|t| SKIP_WS_TRIM_TAGS.contains(&t))
                    || has_preserve_ws_attr(&c.attrs);
                if !skip {
                    new_c.children = remove_whitespaces(&c.children);
                }
                new_c
                    .attrs
                    .retain(|a| a.name != PRESERVE_WS_ATTR_NAME);
                out.push(html::Node::Component(Box::new(new_c)));
            }
            html::Node::Block(b) => {
                let mut new_b = (**b).clone();
                new_b.children = remove_whitespaces(&b.children);
                out.push(html::Node::Block(Box::new(new_b)));
            }
            html::Node::Expansion(e) => {
                let mut new_e = (**e).clone();
                new_e.cases = new_e
                    .cases
                    .into_iter()
                    .map(|mut case| {
                        case.expression = remove_whitespaces(&case.expression);
                        case
                    })
                    .collect();
                out.push(html::Node::Expansion(Box::new(new_e)));
            }
            // Comments, attributes, directives, let-declarations, block params, and orphan
            // expansion cases pass through unchanged (visitComment/visitLetDeclaration/etc.).
            other => out.push(other.clone()),
        }
    }
    out
}

/// `isStyleUrlResolvable`. NOTE(port): replace with `crate::style_url_resolver`.
fn is_style_url_resolvable(url: &str) -> bool {
    if url.is_empty() || url.starts_with("//") {
        return false;
    }
    // Approximation of `URL_WITH_SCHEMA_REGEXP`: data URIs and protocol-absolute URLs are not
    // resolvable; relative/absolute paths are.
    !(url.starts_with("data:") || url.contains("://"))
        || url.starts_with("./")
        || url.starts_with('/')
}

/// Build a `ParseError` from an offset span (used where only offset spans are available).
fn offset_error(msg: &str, span: OffsetSpan) -> ParseError {
    let file = html::ParseSourceFile::new(String::new(), String::new());
    ParseError {
        span: ParseSourceSpan::new(
            html::ParseLocation::new(file.clone(), span.start as usize, 0, 0),
            html::ParseLocation::new(file, span.end as usize, 0, 0),
        ),
        msg: msg.to_string(),
        level: ParseErrorLevel::Error,
        element_name: None,
    }
}

// ===========================================================================
// preparseElement — local placeholder. NOTE(port): replace with template_preparser.
// ===========================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreparsedElementType {
    NgContent,
    Style,
    Stylesheet,
    Script,
    Other,
}

struct PreparsedElement {
    kind: PreparsedElementType,
    select_attr: String,
    href_attr: String,
    non_bindable: bool,
}

/// `preparseElement` — classify an element by tag name and inspect a few well-known attributes
/// (`select`, `href`/`rel=stylesheet`, `ngNonBindable`).
fn preparse_element(
    name: &str,
    attrs: &[html::Attribute],
    _children: &[html::Node],
) -> PreparsedElement {
    let mut select_attr = "*".to_string();
    let mut href_attr = String::new();
    let mut rel_attr = String::new();
    let mut non_bindable = false;

    for attr in attrs {
        match attr.name.to_lowercase().as_str() {
            "select" => {
                select_attr = if attr.value.is_empty() {
                    "*".to_string()
                } else {
                    attr.value.clone()
                }
            }
            "href" => href_attr = attr.value.clone(),
            "rel" => rel_attr = attr.value.clone(),
            "ngnonbindable" => non_bindable = true,
            _ => {}
        }
    }

    let kind = match name.to_lowercase().as_str() {
        "ng-content" => PreparsedElementType::NgContent,
        "style" => PreparsedElementType::Style,
        "script" => PreparsedElementType::Script,
        "link" if rel_attr == "stylesheet" => PreparsedElementType::Stylesheet,
        _ => PreparsedElementType::Other,
    };

    PreparsedElement {
        kind,
        select_attr,
        href_attr,
        non_bindable,
    }
}

// ===========================================================================
// @defer local placeholder.
// NOTE(port): replace with `deferred::create_deferred_block` once that module lands. Until then,
// `@defer` (and the connected `@placeholder`/`@loading`/`@error`) collapse to an UnknownBlock.
// ===========================================================================

fn create_deferred_block_local(
    block: &html::Block,
    _connected: &[html::Block],
) -> (Option<t::Node>, Vec<ParseError>) {
    (
        Some(t::Node::UnknownBlock(t::UnknownBlock {
            name: block.name.clone(),
            source_span: to_offset_span(&block.source_span),
            name_span: to_offset_span(&block.name_span),
        })),
        vec![ParseError {
            span: block.source_span.clone(),
            msg: "@defer block transform not yet ported".to_string(),
            level: ParseErrorLevel::Warning,
            element_name: None,
        }],
    )
}

// ===========================================================================
// NonBindableVisitor — verbatim/passthrough visitor for ngNonBindable subtrees.
// ===========================================================================

struct NonBindableVisitor;

impl NonBindableVisitor {
    fn visit_all(&mut self, nodes: &[html::Node]) -> Vec<t::Node> {
        let mut out = Vec::new();
        for node in nodes {
            match node {
                html::Node::Element(e) => {
                    if let Some(n) = self.visit_element(e) {
                        out.push(n);
                    }
                }
                html::Node::Text(text) => out.push(t::Node::Text(t::Text {
                    value: text.value.clone(),
                    source_span: to_offset_span(&text.source_span),
                })),
                html::Node::Comment(_)
                | html::Node::Expansion(_)
                | html::Node::ExpansionCase(_)
                | html::Node::Attribute(_)
                | html::Node::BlockParameter(_)
                | html::Node::Directive(_) => {}
                html::Node::Block(b) => out.extend(self.visit_block(b)),
                html::Node::Component(c) => out.push(self.visit_component(c)),
                html::Node::LetDeclaration(d) => out.push(t::Node::Text(t::Text {
                    value: format!("@let {} = {};", d.name, d.value),
                    source_span: to_offset_span(&d.source_span),
                })),
            }
        }
        out
    }

    fn visit_element(&mut self, ast: &html::Element) -> Option<t::Node> {
        let preparsed = preparse_element(&ast.name, &ast.attrs, &ast.children);
        if matches!(
            preparsed.kind,
            PreparsedElementType::Script
                | PreparsedElementType::Style
                | PreparsedElementType::Stylesheet
        ) {
            return None;
        }
        let children = self.visit_all(&ast.children);
        Some(t::Node::Element(t::Element {
            name: ast.name.clone(),
            attributes: ast.attrs.iter().map(visit_attribute).collect(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            directives: Vec::new(),
            children,
            references: Vec::new(),
            is_self_closing: ast.is_self_closing,
            source_span: to_offset_span(&ast.source_span),
            start_source_span: to_offset_span(&ast.start_source_span),
            end_source_span: to_offset_span_opt(&ast.end_source_span),
            is_void: ast.is_void,
            i18n: None,
        }))
    }

    fn visit_component(&mut self, ast: &html::Component) -> t::Node {
        let children = self.visit_all(&ast.children);
        t::Node::Element(t::Element {
            name: ast.full_name.clone(),
            attributes: ast.attrs.iter().map(visit_attribute).collect(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            directives: Vec::new(),
            children,
            references: Vec::new(),
            is_self_closing: ast.is_self_closing,
            source_span: to_offset_span(&ast.source_span),
            start_source_span: to_offset_span(&ast.start_source_span),
            end_source_span: to_offset_span_opt(&ast.end_source_span),
            is_void: false,
            i18n: None,
        })
    }

    fn visit_block(&mut self, block: &html::Block) -> Vec<t::Node> {
        // Treat the opening/closing tags of the block as plain text (as if tokenizeBlocks off).
        let mut nodes = vec![t::Node::Text(t::Text {
            value: block.start_source_span.to_source_string(),
            source_span: to_offset_span(&block.start_source_span),
        })];
        nodes.extend(self.visit_all(&block.children));
        if let Some(end) = &block.end_source_span {
            nodes.push(t::Node::Text(t::Text {
                value: end.to_source_string(),
                source_span: to_offset_span(end),
            }));
        }
        nodes
    }
}

// ===========================================================================
// BindingParser — local placeholder wrapping the low-level expression Parser.
// NOTE(port): replace with `crate::template::binding_parser::BindingParser`.
// ===========================================================================

/// A minimal `BindingParser` that accumulates `ParseError`s and delegates expression parsing to
/// the ported [`crate::expression::parser::Parser`]. Reproduces the subset of methods the
/// transform calls. Spans are converted to the offset-based form carried by
/// `ParsedProperty`/`ParsedEvent`/`BoundElementProperty`.
pub struct BindingParser {
    parser: ExprParser,
    errors: Vec<ParseError>,
}

impl Default for BindingParser {
    fn default() -> Self {
        BindingParser {
            parser: ExprParser::default(),
            errors: Vec::new(),
        }
    }
}

impl BindingParser {
    pub fn new() -> Self {
        BindingParser::default()
    }

    /// Drain the accumulated errors (mirrors reading `bindingParser.errors`).
    pub fn take_errors(&mut self) -> Vec<ParseError> {
        std::mem::take(&mut self.errors)
    }

    fn record_expr_errors(&mut self, errors: &[ExprParseError], html_span: &ParseSourceSpan) {
        for e in errors {
            self.errors.push(ParseError {
                span: html_span.clone(),
                msg: e.msg.clone(),
                level: ParseErrorLevel::Error,
                element_name: None,
            });
        }
    }

    /// `parseInterpolation`. Returns `None` when there is no `{{ }}`.
    pub fn parse_interpolation(
        &mut self,
        value: &str,
        span: &ParseSourceSpan,
    ) -> Option<AstWithSource> {
        let offset_span = to_offset_span(span);
        let result = self
            .parser
            .parse_interpolation(value, offset_span, abs_offset(span));
        if let Some(aws) = &result {
            let errs = aws.errors.clone();
            self.record_expr_errors(&errs, span);
        }
        result
    }

    /// `parseBinding`.
    pub fn parse_binding(
        &mut self,
        value: &str,
        _is_host_binding: bool,
        span: &ParseSourceSpan,
    ) -> AstWithSource {
        let offset_span = to_offset_span(span);
        let aws = self.parser.parse_binding(value, offset_span, abs_offset(span));
        let errs = aws.errors.clone();
        self.record_expr_errors(&errs, span);
        aws
    }

    /// `parsePropertyBinding` — append a `ParsedProperty` (Default or TwoWay).
    #[allow(clippy::too_many_arguments)]
    pub fn parse_property_binding(
        &mut self,
        name: &str,
        value: &str,
        is_two_way: bool,
        source_span: &ParseSourceSpan,
        absolute_offset: i32,
        value_span: &Option<ParseSourceSpan>,
        parsed_properties: &mut Vec<ParsedProperty>,
        key_span: &ParseSourceSpan,
    ) {
        let span = value_span.as_ref().unwrap_or(source_span);
        let offset_span = to_offset_span(span);
        let aws = self.parser.parse_binding(value, offset_span, absolute_offset);
        let errs = aws.errors.clone();
        self.record_expr_errors(&errs, source_span);
        let ty = if is_two_way {
            ParsedPropertyType::TwoWay
        } else {
            ParsedPropertyType::Default
        };
        parsed_properties.push(ParsedProperty {
            name: name.to_string(),
            expression: aws,
            ty,
            source_span: to_offset_span(source_span),
            key_span: to_offset_span(key_span),
            value_span: to_offset_span_opt(value_span),
        });
    }

    /// `parsePropertyInterpolation` — append a `ParsedProperty` iff the value has `{{ }}`.
    #[allow(clippy::too_many_arguments)]
    pub fn parse_property_interpolation(
        &mut self,
        name: &str,
        value: &str,
        source_span: &ParseSourceSpan,
        value_span: &Option<ParseSourceSpan>,
        parsed_properties: &mut Vec<ParsedProperty>,
        key_span: &ParseSourceSpan,
    ) -> bool {
        let span = value_span.as_ref().unwrap_or(source_span);
        let offset_span = to_offset_span(span);
        match self
            .parser
            .parse_interpolation(value, offset_span, abs_offset(span))
        {
            Some(aws) => {
                let errs = aws.errors.clone();
                self.record_expr_errors(&errs, source_span);
                parsed_properties.push(ParsedProperty {
                    name: name.to_string(),
                    expression: aws,
                    ty: ParsedPropertyType::Default,
                    source_span: to_offset_span(source_span),
                    key_span: to_offset_span(key_span),
                    value_span: to_offset_span_opt(value_span),
                });
                true
            }
            None => false,
        }
    }

    /// `parseLiteralAttr` — append a literal (`@`-prefixed) `ParsedProperty`.
    #[allow(clippy::too_many_arguments)]
    pub fn parse_literal_attr(
        &mut self,
        name: &str,
        value: &str,
        source_span: &ParseSourceSpan,
        absolute_offset: i32,
        value_span: &Option<ParseSourceSpan>,
        parsed_properties: &mut Vec<ParsedProperty>,
        key_span: &ParseSourceSpan,
    ) {
        let location = format!("{}", source_span.start.offset);
        let aws = self
            .parser
            .wrap_literal_primitive(Some(value), location, absolute_offset);
        parsed_properties.push(ParsedProperty {
            name: name.to_string(),
            expression: aws,
            ty: ParsedPropertyType::LiteralAttr,
            source_span: to_offset_span(source_span),
            key_span: to_offset_span(key_span),
            value_span: to_offset_span_opt(value_span),
        });
    }

    /// `parseEvent` — append a `ParsedEvent` (Regular or TwoWay).
    #[allow(clippy::too_many_arguments)]
    pub fn parse_event(
        &mut self,
        name: &str,
        value: &str,
        is_assignment_event: bool,
        source_span: &ParseSourceSpan,
        handler_span: &ParseSourceSpan,
        events: &mut Vec<ParsedEvent>,
        key_span: &ParseSourceSpan,
    ) {
        let offset_handler = to_offset_span(handler_span);
        let aws = self
            .parser
            .parse_action(value, offset_handler, abs_offset(handler_span));
        let errs = aws.errors.clone();
        self.record_expr_errors(&errs, source_span);
        let ty = if is_assignment_event {
            ParsedEventType::TwoWay
        } else {
            ParsedEventType::Regular
        };
        events.push(ParsedEvent {
            name: name.to_string(),
            target_or_phase: None,
            ty,
            handler: aws,
            source_span: to_offset_span(source_span),
            handler_span: to_offset_span(handler_span),
            key_span: to_offset_span(key_span),
        });
    }

    /// `createBoundElementProperty` — map a `ParsedProperty` to a `BoundElementProperty`.
    pub fn create_bound_element_property(
        &mut self,
        _element_name: Option<&str>,
        prop: &ParsedProperty,
        _skip_validation: bool,
        _map_property_name: bool,
    ) -> BoundElementProperty {
        // Recognize the class/style/attr/animation prefixes; otherwise a plain property.
        // NOTE(port): the full BindingParser also resolves security contexts and units; we keep
        // the binding-type classification and leave security None / unit None.
        let (ty, name) = classify_bound_property(&prop.name);
        let kind = if prop.ty == ParsedPropertyType::TwoWay {
            BindingType::TwoWay
        } else {
            ty
        };
        BoundElementProperty {
            name,
            ty: kind,
            security_context: SecurityContext::None,
            value: prop.expression.clone(),
            unit: None,
            source_span: prop.source_span.clone(),
            key_span: Some(prop.key_span.clone()),
            value_span: prop.value_span.clone(),
        }
    }

    /// `parseInlineTemplateBinding` — microsyntax (`*ngFor="..."`). Splits the result into
    /// expression bindings (-> `ParsedProperty`) and variable bindings (-> `ParsedVariable`).
    #[allow(clippy::too_many_arguments)]
    pub fn parse_inline_template_binding(
        &mut self,
        template_key: &str,
        template_value: &str,
        source_span: &ParseSourceSpan,
        absolute_value_offset: i32,
        parsed_properties: &mut Vec<ParsedProperty>,
        parsed_variables: &mut Vec<ParsedVariable>,
    ) {
        let offset_span = to_offset_span(source_span);
        let absolute_key_offset = source_span.start.offset as i32;
        let result = self.parser.parse_template_bindings(
            template_key,
            template_value,
            offset_span,
            absolute_key_offset,
            absolute_value_offset,
        );
        for e in &result.errors {
            self.errors.push(ParseError {
                span: source_span.clone(),
                msg: e.msg.clone(),
                level: ParseErrorLevel::Error,
                element_name: None,
            });
        }

        use crate::expression::ast::TemplateBinding;
        let prop_span = to_offset_span(source_span);
        for binding in result.template_bindings {
            match binding {
                TemplateBinding::Variable { key, value, .. } => {
                    parsed_variables.push(ParsedVariable {
                        name: key.source.clone(),
                        value: value.map(|v| v.source).unwrap_or_default(),
                        source_span: prop_span.clone(),
                        key_span: prop_span.clone(),
                        value_span: None,
                    });
                }
                TemplateBinding::Expression { key, value, .. } => {
                    let aws = value.unwrap_or_else(|| {
                        self.parser
                            .wrap_literal_primitive(None, String::new(), absolute_value_offset)
                    });
                    parsed_properties.push(ParsedProperty {
                        name: key.source.clone(),
                        expression: aws,
                        ty: ParsedPropertyType::Default,
                        source_span: prop_span.clone(),
                        key_span: prop_span.clone(),
                        value_span: None,
                    });
                }
            }
        }
    }
}

/// Recognize the `class.x` / `style.x` / `attr.x` / `animate.*` prefixes; default to a plain
/// property binding. Mirrors the dispatch in `BindingParser.createBoundElementProperty`.
fn classify_bound_property(name: &str) -> (BindingType, String) {
    if let Some(rest) = name.strip_prefix("class.") {
        (BindingType::Class, rest.to_string())
    } else if let Some(rest) = name.strip_prefix("style.") {
        (BindingType::Style, rest.to_string())
    } else if let Some(rest) = name.strip_prefix("attr.") {
        (BindingType::Attribute, rest.to_string())
    } else if name.starts_with("animate.") {
        (BindingType::Animation, name.to_string())
    } else {
        (BindingType::Property, name.to_string())
    }
}

// ===========================================================================
// Tests.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ml_parser;

    fn parse_html(src: &str) -> Vec<ml_parser::Node> {
        let result = ml_parser::parse(src, "test.html");
        assert!(
            result.errors.is_empty(),
            "html parse errors: {:?}",
            result.errors
        );
        result.root_nodes
    }

    #[test]
    fn div_with_bracket_input_paren_event_and_interpolation() {
        let nodes = parse_html(r#"<div [id]="x" (click)="go()">{{ name }}</div>"#);
        let mut bp = BindingParser::new();
        let result = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());

        assert_eq!(result.nodes.len(), 1, "expected a single root element");
        let element = match &result.nodes[0] {
            t::Node::Element(e) => e,
            other => panic!("expected Element, got {other:?}"),
        };
        assert_eq!(element.name, "div");

        // [id] -> a bound input named "id".
        assert_eq!(element.inputs.len(), 1, "expected one bound input");
        assert_eq!(element.inputs[0].name, "id");
        assert_eq!(element.inputs[0].kind, BindingType::Property);

        // (click) -> a bound output named "click".
        assert_eq!(element.outputs.len(), 1, "expected one bound output");
        assert_eq!(element.outputs[0].name, "click");
        assert_eq!(element.outputs[0].kind, ParsedEventType::Regular);

        // Neither binding leaks into static attributes.
        assert!(
            element.attributes.is_empty(),
            "bindings should not appear as static attributes: {:?}",
            element.attributes
        );

        // The text child is interpolated -> BoundText.
        assert_eq!(element.children.len(), 1);
        assert!(
            matches!(element.children[0], t::Node::BoundText(_)),
            "expected interpolated text to become BoundText, got {:?}",
            element.children[0]
        );
    }

    #[test]
    fn static_attribute_stays_static() {
        let nodes = parse_html(r#"<div class="box">text</div>"#);
        let mut bp = BindingParser::new();
        let result = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());
        let element = match &result.nodes[0] {
            t::Node::Element(e) => e,
            other => panic!("expected Element, got {other:?}"),
        };
        assert_eq!(element.attributes.len(), 1);
        assert_eq!(element.attributes[0].name, "class");
        assert_eq!(element.attributes[0].value, "box");
        assert!(element.inputs.is_empty());
        assert!(matches!(element.children[0], t::Node::Text(_)));
    }

    #[test]
    fn hash_reference_becomes_reference() {
        let nodes = parse_html(r#"<input #ref>"#);
        let mut bp = BindingParser::new();
        let result = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());
        let element = match &result.nodes[0] {
            t::Node::Element(e) => e,
            other => panic!("expected Element, got {other:?}"),
        };
        assert_eq!(element.references.len(), 1);
        assert_eq!(element.references[0].name, "ref");
    }

    #[test]
    fn if_block_end_to_end() {
        let nodes = parse_html("@if (show) {<span>hi</span>}");
        let mut bp = BindingParser::new();
        let result = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());

        let if_block = result
            .nodes
            .iter()
            .find_map(|n| match n {
                t::Node::IfBlock(b) => Some(b),
                _ => None,
            })
            .expect("expected an IfBlock");

        assert_eq!(if_block.branches.len(), 1, "expected a single @if branch");
        let branch = &if_block.branches[0];
        assert!(branch.expression.is_some(), "the @if branch has a condition");
        assert!(
            branch
                .children
                .iter()
                .any(|c| matches!(c, t::Node::Element(e) if e.name == "span")),
            "branch children: {:?}",
            branch.children
        );
    }

    #[test]
    fn if_else_block_two_branches() {
        let nodes = parse_html("@if (a) {<b>x</b>} @else {<i>y</i>}");
        let mut bp = BindingParser::new();
        let result = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());
        let if_block = result
            .nodes
            .iter()
            .find_map(|n| match n {
                t::Node::IfBlock(b) => Some(b),
                _ => None,
            })
            .expect("expected an IfBlock");
        assert_eq!(if_block.branches.len(), 2, "@if + @else => two branches");
        assert!(if_block.branches[0].expression.is_some());
        assert!(
            if_block.branches[1].expression.is_none(),
            "the @else branch has no condition"
        );
        let if_count = result
            .nodes
            .iter()
            .filter(|n| matches!(n, t::Node::IfBlock(_)))
            .count();
        assert_eq!(if_count, 1);
        assert!(
            !result
                .nodes
                .iter()
                .any(|n| matches!(n, t::Node::UnknownBlock(_))),
            "the @else block must be consumed, not emitted as UnknownBlock"
        );
    }

    #[test]
    fn whitespace_only_text_between_elements_is_trimmed() {
        // A template with newlines/indentation between elements must yield the same r3_ast node
        // count as the whitespace-free form (insignificant whitespace text nodes are dropped).
        let pretty = "<div>\n  <span>a</span>\n  <span>b</span>\n</div>";
        let compact = "<div><span>a</span><span>b</span></div>";

        let mut bp1 = BindingParser::new();
        let pretty_nodes = parse_html(pretty);
        let pretty_res =
            html_ast_to_render3_ast(&pretty_nodes, &mut bp1, Render3ParseOptions::default());

        let mut bp2 = BindingParser::new();
        let compact_nodes = parse_html(compact);
        let compact_res =
            html_ast_to_render3_ast(&compact_nodes, &mut bp2, Render3ParseOptions::default());

        assert_eq!(pretty_res.nodes.len(), compact_res.nodes.len());
        assert_eq!(pretty_res.nodes.len(), 1);

        let pretty_div = match &pretty_res.nodes[0] {
            t::Node::Element(e) => e,
            other => panic!("expected Element, got {other:?}"),
        };
        let compact_div = match &compact_res.nodes[0] {
            t::Node::Element(e) => e,
            other => panic!("expected Element, got {other:?}"),
        };
        // Both should have exactly the two <span> children; the whitespace text nodes are gone.
        assert_eq!(pretty_div.children.len(), compact_div.children.len());
        assert_eq!(pretty_div.children.len(), 2);
        assert!(
            pretty_div
                .children
                .iter()
                .all(|c| matches!(c, t::Node::Element(e) if e.name == "span")),
            "expected only span children, got {:?}",
            pretty_div.children
        );
    }

    #[test]
    fn significant_text_and_internal_runs_are_preserved_and_collapsed() {
        // A non-blank text node survives; internal whitespace runs collapse to a single space.
        let nodes = parse_html("<p>Hello    world</p>");
        let mut bp = BindingParser::new();
        let result = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());
        let p = match &result.nodes[0] {
            t::Node::Element(e) => e,
            other => panic!("expected Element, got {other:?}"),
        };
        assert_eq!(p.children.len(), 1);
        match &p.children[0] {
            t::Node::Text(t) => assert_eq!(t.value, "Hello world"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn whitespace_preserved_in_pre_tag() {
        // Inside <pre>, whitespace-only text nodes between elements are NOT dropped.
        let nodes = parse_html("<pre>\n  <span>a</span>\n</pre>");
        let mut bp = BindingParser::new();
        let result = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());
        let pre = match &result.nodes[0] {
            t::Node::Element(e) => e,
            other => panic!("expected Element, got {other:?}"),
        };
        // text("\n  "), span, text("\n") => 3 children preserved.
        assert_eq!(pre.children.len(), 3, "children: {:?}", pre.children);
    }

    #[test]
    fn comment_collection_toggles_with_option() {
        let nodes = parse_html("<!-- hi -->");
        let mut bp = BindingParser::new();

        let off = html_ast_to_render3_ast(&nodes, &mut bp, Render3ParseOptions::default());
        assert!(off.comment_nodes.is_none());

        let mut bp2 = BindingParser::new();
        let on = html_ast_to_render3_ast(
            &nodes,
            &mut bp2,
            Render3ParseOptions {
                collect_comment_nodes: true,
            },
        );
        assert_eq!(on.comment_nodes.as_ref().map(|c| c.len()), Some(1));
    }
}
