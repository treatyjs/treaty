//! JSX template lowering: turn an OXC JSX element/fragment tree into an Angular template HTML
//! string that the shared render3 backend (`sfc::compile_from_parts`) can consume.
//!
//! The lowering is a recursive visitor over the JSX AST that produces Angular template markup:
//!
//! Elements / components
//!   * HTML element tags (`<div>`) pass through verbatim.
//!   * Component tags (Capitalized, `<Foo>`) pass through by tag name — render3's selectorless
//!     binder resolves the component reference by class name, so no `imports` array is needed.
//!   * A JSX fragment (`<>…</>`) renders its children with no wrapper element.
//!
//! Children
//!   * JSX text is preserved (with JSX-insignificant whitespace trimmed, see [`lower_children`]).
//!   * a `{expr}` expression container in child position becomes `{{ expr }}` interpolation.
//!   * children recurse, so nested elements/fragments/interpolations compose.
//!
//! Attributes (see [`lower_attribute`])
//!   * `name="v"`            → `name="v"` (string attribute passes through)
//!   * `prop={expr}`         → `[prop]="expr"` (Angular property binding)
//!   * `attr` (shorthand)    → `attr=""` for a known boolean DOM attribute, else `[attr]="true"`
//!   * `class="x"`           → `class="x"`
//!   * `class={expr}`        → `[class]="expr"`
//!   * `class={{ a: x }}`    → `[class.a]="x"` per key
//!   * `class={['a', x]}`    → `[class]="['a', x].join(' ')"` (array joined to a class string)
//!   * `className=…`         → treated exactly as `class` ([`super::directives::map_attribute_name`])
//!   * `style={{ color: x }}`→ `[style.color]="x"` per key
//!   * `style="…"`           → `style="…"`
//!   * `onClick={h}`         → `(click)="h($event)"` (event name = handler prop minus `on`,
//!     lowercased; see [`super::directives::event_name`])
//!   * `{...obj}` spread      → `[ngSpreadBindings]="obj"` (documented mapping, see [`lower_spread`])

use oxc_ast::ast::{
    Expression, JSXAttributeItem, JSXAttributeName, JSXAttributeValue, JSXChild, JSXElement,
    JSXElementName, JSXExpression, JSXFragment, ObjectExpression, ObjectPropertyKind,
};

/// Lower a JSX element into its Angular template HTML string.
///
/// A structural directive (`*highlight={expr}`, see [`super::directives`]) wraps the host element
/// in an `<ng-template>` carrying the structural binding, exactly as `*ngIf` desugars; all such
/// directives on the element are collected and applied as nested wrappers (outermost = first).
pub fn lower_element(element: &JSXElement, source: &str) -> String {
    // Split out any structural directives (`*foo`) — they wrap the host rather than annotate it.
    // Each carries its directive class, the structural binding name (the directive's primary input),
    // and the bound expression text (when written with a value).
    let mut structurals: Vec<StructuralDirective> = Vec::new();
    for attr in &element.opening_element.attributes {
        if let JSXAttributeItem::Attribute(a) = attr {
            let raw_name = attribute_name(&a.name);
            if let Some(app) =
                super::directives::classify_attribute(&raw_name, a.value.is_some())
            {
                if app.structural {
                    super::directives::register_directive_reference(&app.class_name);
                    structurals.push(StructuralDirective {
                        input: app
                            .input_name
                            .unwrap_or_else(|| super::directives::primary_input_name_of(&app.class_name)),
                        value: structural_value_text(a, source),
                    });
                }
            }
        }
    }

    let host = lower_element_host(element, source);

    // Wrap the host in one `<ng-template>` per structural directive (innermost host first), exactly
    // as `*ngIf` / `*ngFor` desugar to a template carrying the structural input binding.
    let mut wrapped = host;
    for structural in structurals.into_iter().rev() {
        let mut tpl = String::from("<ng-template");
        match structural.value {
            Some(expr) => {
                tpl.push_str(" [");
                tpl.push_str(&structural.input);
                tpl.push_str("]=\"");
                tpl.push_str(expr.trim());
                tpl.push('"');
            }
            None => {
                tpl.push(' ');
                tpl.push_str(&structural.input);
                tpl.push_str("=\"\"");
            }
        }
        tpl.push('>');
        tpl.push_str(&wrapped);
        tpl.push_str("</ng-template>");
        wrapped = tpl;
    }
    wrapped
}

/// A structural directive lifted off a host element: its structural binding name and the bound
/// expression text (when the directive was written with a value, `*foo={expr}`).
struct StructuralDirective {
    input: String,
    value: Option<String>,
}

/// The verbatim expression text for a structural directive's value (`*foo={expr}` → `expr`), or
/// `None` for a value-less structural directive (`*foo`) or a string-literal value (rare; treated as
/// no dynamic binding).
fn structural_value_text(attr: &oxc_ast::ast::JSXAttribute, source: &str) -> Option<String> {
    match &attr.value {
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            expression_text(&container.expression, source)
        }
        _ => None,
    }
}

/// Lower the host element itself (open tag + attributes + children), without any structural-directive
/// wrapping — that is applied by [`lower_element`].
fn lower_element_host(element: &JSXElement, source: &str) -> String {
    let tag = element_name(&element.opening_element.name);
    let mut out = String::new();
    out.push('<');
    out.push_str(&tag);

    for attr in &element.opening_element.attributes {
        match attr {
            JSXAttributeItem::Attribute(attr) => {
                // Structural directives are handled by `lower_element`; skip them on the host.
                let raw_name = attribute_name(&attr.name);
                if let Some(app) =
                    super::directives::classify_attribute(&raw_name, attr.value.is_some())
                {
                    if app.structural {
                        continue;
                    }
                }
                lower_attribute(&mut out, attr, source);
            }
            JSXAttributeItem::SpreadAttribute(spread) => {
                lower_spread(&mut out, &spread.argument, source);
            }
        }
    }

    // A self-closing element with no children: `<input />`. Angular (HTML) does not honour
    // self-closing for non-void elements, but render3's parser accepts the explicit close, so emit
    // an open/close pair for elements that carry children and a self-close only when truly empty.
    if element.closing_element.is_none() && element.children.is_empty() {
        out.push_str(" />");
        return out;
    }

    out.push('>');
    out.push_str(&lower_children(&element.children, source));
    out.push_str("</");
    out.push_str(&tag);
    out.push('>');
    out
}

/// Lower a JSX fragment (`<>…</>`) into the HTML of its children (fragments have no host element).
pub fn lower_fragment(fragment: &JSXFragment, source: &str) -> String {
    lower_children(&fragment.children, source)
}

/// Lower an arbitrary expression that appears in a "renders to markup" position (a control-flow
/// branch body, a `.map` callback return, a ternary arm) into Angular template HTML.
///
/// The expression is the lowered content of a block, so it is recursively transpiled:
///   * a JSX element / fragment lowers through [`lower_element`] / [`lower_fragment`] (so nested
///     elements, attributes and further control flow are fully handled);
///   * a parenthesized expression unwraps to its inner expression;
///   * a string literal renders as literal text; any other primitive (number/template/identifier)
///     renders as an interpolation `{{ expr }}` so dynamic values still reach the DOM;
///   * a nested control-flow expression (`&&` / ternary / `.map`) recurses through
///     [`super::control_flow::lower_child_expression`].
pub(crate) fn lower_renderable_expression(expression: &Expression, source: &str) -> String {
    match expression {
        Expression::JSXElement(el) => lower_element(el, source),
        Expression::JSXFragment(frag) => lower_fragment(frag, source),
        Expression::ParenthesizedExpression(inner) => {
            lower_renderable_expression(&inner.expression, source)
        }
        Expression::StringLiteral(s) => collapse_jsx_text(s.value.as_str()),
        other => {
            // Nested control flow (`a && <X/>`, `c ? <A/> : <B/>`, `xs.map(...)`) inside a branch
            // body recurses; everything else becomes an interpolation so dynamic text survives.
            if let Some(block) = super::control_flow::lower_expression(other, source) {
                block
            } else {
                let text = expression_source(other, source);
                format!("{{{{ {} }}}}", text.trim())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Children.
// ---------------------------------------------------------------------------

/// Lower a list of JSX children into concatenated Angular template HTML.
///
/// JSX whitespace semantics: a text node that is entirely whitespace and contains a newline is
/// "insignificant" (it is layout between elements) and is dropped; an all-whitespace text node with
/// no newline (e.g. a single space between two inline elements) is significant and kept. For a text
/// node with content, JSX trims leading/trailing whitespace on lines that border a newline and
/// collapses interior newlines+indentation to a single space; we implement that collapse so
/// `\n      Hello\n    ` renders as `Hello`.
///
/// `{expr}` expression containers are routed through [`super::control_flow`] first: a JSX
/// `cond && <X/>`, a `cond ? <A/> : <B/>` ternary, or a `list.map(...)` / `list.forEach(...)`
/// render becomes Angular block control flow (`@if` / `@for`). Anything else stays an interpolation.
pub(crate) fn lower_children(children: &[JSXChild], source: &str) -> String {
    let mut out = String::new();
    for child in children {
        match child {
            JSXChild::Text(text) => {
                out.push_str(&collapse_jsx_text(text.value.as_str()));
            }
            JSXChild::Element(el) => out.push_str(&lower_element(el, source)),
            JSXChild::Fragment(frag) => out.push_str(&lower_fragment(frag, source)),
            JSXChild::ExpressionContainer(container) => {
                // Control-flow expressions (`&&`, ternary, `.map`) lower to Angular `@if`/`@for`.
                if let Some(block) =
                    super::control_flow::lower_child_expression(&container.expression, source)
                {
                    out.push_str(&block);
                } else if let Some(text) = expression_text(&container.expression, source) {
                    out.push_str("{{ ");
                    out.push_str(text.trim());
                    out.push_str(" }}");
                }
            }
            // `{...children}` in child position has no Angular template equivalent (there is no
            // dynamic child-list spread in a template); it is dropped.
            JSXChild::Spread(_) => {}
        }
    }
    out
}

/// Collapse a JSX text node per JSX whitespace rules and return the rendered text.
///
/// Rules applied (matching how JSX/TSX compile text):
///   * Lines are split on `\n`; on each newline boundary the surrounding whitespace is trimmed.
///   * Non-empty lines are re-joined with a single space.
///   * A text node that is all whitespace yields `""` if it spans a newline (insignificant layout),
///     otherwise it is kept verbatim (a meaningful inline space).
fn collapse_jsx_text(value: &str) -> String {
    if value.trim().is_empty() {
        // All-whitespace: significant only when it does not span a line break.
        return if value.contains('\n') {
            String::new()
        } else {
            value.to_string()
        };
    }

    // Split on newlines, trim whitespace introduced by source indentation on the bordering lines,
    // and re-join non-empty pieces with a single space — exactly the JSX text collapse.
    let mut pieces: Vec<&str> = Vec::new();
    for (idx, line) in value.split('\n').enumerate() {
        let is_first = idx == 0;
        // The first line keeps its leading whitespace only if there was no newline before it; but
        // since we split on '\n', any line after the first had a preceding newline. JSX trims the
        // edge that touches a newline, so trim the start of every line except the first and the end
        // of every line except the last. Simplest faithful behaviour: trim both edges of interior
        // lines, and for the first/last line trim only the newline-adjacent edge.
        let trimmed = if is_first {
            // Trailing edge touches the following '\n' (if any); leading edge is the literal start.
            line.trim_end()
        } else {
            line.trim()
        };
        if !trimmed.is_empty() {
            pieces.push(trimmed);
        }
    }
    // Preserve a single leading/trailing space if the original had significant inline whitespace
    // that did not border a newline. The common authoring cases ("Hello", " {x} ") are handled by
    // the join below; bordering-newline whitespace has already been trimmed away.
    pieces.join(" ")
}

// ---------------------------------------------------------------------------
// Attributes.
// ---------------------------------------------------------------------------

/// Lower a single JSX attribute onto `out` (which already holds the open tag up to this point).
fn lower_attribute(out: &mut String, attr: &oxc_ast::ast::JSXAttribute, source: &str) {
    let raw_name = attribute_name(&attr.name);

    // Event handler: `onX={h}` → `(x)="h($event)"`. Recognized before the name is mapped so the
    // `on`-prefix is intact.
    if let Some(event) = super::directives::event_name(&raw_name) {
        if let Some(JSXAttributeValue::ExpressionContainer(container)) = &attr.value {
            if let Some(text) = expression_text(&container.expression, source) {
                lower_event(out, &event, text.trim());
            }
        }
        return;
    }

    // Directive application (non-structural). The three syntaxes — `use:tooltip`, `Tooltip`, and a
    // bare lowercase attribute matching a known directive — apply the directive on the element and,
    // when carrying a value, bind its primary input. Structural directives (`*foo`) are handled by
    // `lower_element` (they wrap the host) and never reach here.
    if let Some(app) = super::directives::classify_attribute(&raw_name, attr.value.is_some()) {
        if !app.structural {
            lower_directive(out, &app, attr, source);
            return;
        }
    }

    let name = super::directives::map_attribute_name(&raw_name);

    // `class` / `className` and `style` carry rich object/array forms.
    if name == "class" {
        lower_class_attribute(out, attr, source);
        return;
    }
    if name == "style" {
        lower_style_attribute(out, attr, source);
        return;
    }

    match &attr.value {
        // `name="v"` passes through.
        Some(JSXAttributeValue::StringLiteral(s)) => {
            push_string_attr(out, &name, s.value.as_str());
        }
        // `prop={expr}` → property binding `[prop]="expr"`.
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(text) = expression_text(&container.expression, source) {
                push_property_binding(out, &name, text.trim());
            }
        }
        // `prop=<Element/>` / `prop=<></>` as an attribute value is not meaningful Angular markup;
        // drop it (these are exceedingly rare and have no template equivalent).
        Some(JSXAttributeValue::Element(_)) | Some(JSXAttributeValue::Fragment(_)) => {}
        // Boolean shorthand: `<input disabled />`. A known boolean DOM attribute renders as the
        // bare attribute (`disabled=""`); anything else becomes a `[attr]="true"` binding so the
        // truthy intent survives for component inputs and non-boolean attributes alike.
        None => {
            if is_boolean_attribute(&name) {
                push_string_attr(out, &name, "");
            } else {
                push_property_binding(out, &name, "true");
            }
        }
    }
}

/// Lower the `class` (or `className`) attribute. Supports: a string literal (`class="a b"`), an
/// expression (`[class]="expr"`), an object literal (`[class.key]="value"` per key), and an array
/// literal (joined to a class string via `[class]="[...].join(' ')"`).
fn lower_class_attribute(out: &mut String, attr: &oxc_ast::ast::JSXAttribute, source: &str) {
    match &attr.value {
        Some(JSXAttributeValue::StringLiteral(s)) => {
            push_string_attr(out, "class", s.value.as_str());
        }
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            match &container.expression {
                // `class={{ active: cond }}` → `[class.active]="cond"` per key.
                JSXExpression::ObjectExpression(obj) => {
                    lower_keyed_object(out, "class", obj, source);
                }
                // `class={['a', x]}` → `[class]="['a', x].join(' ')"` (array → class string).
                JSXExpression::ArrayExpression(_) => {
                    if let Some(text) = expression_text(&container.expression, source) {
                        push_property_binding(
                            out,
                            "class",
                            &format!("{}.join(' ')", text.trim()),
                        );
                    }
                }
                // `class={expr}` → `[class]="expr"`.
                _ => {
                    if let Some(text) = expression_text(&container.expression, source) {
                        push_property_binding(out, "class", text.trim());
                    }
                }
            }
        }
        // Bare `class` shorthand is meaningless; emit an empty class for stability.
        None => push_string_attr(out, "class", ""),
        _ => {}
    }
}

/// Lower the `style` attribute. Supports: a string literal (`style="..."`), an object literal
/// (`[style.prop]="value"` per key), and a general expression (`[style]="expr"`).
fn lower_style_attribute(out: &mut String, attr: &oxc_ast::ast::JSXAttribute, source: &str) {
    match &attr.value {
        Some(JSXAttributeValue::StringLiteral(s)) => {
            push_string_attr(out, "style", s.value.as_str());
        }
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            match &container.expression {
                // `style={{ color: c }}` → `[style.color]="c"` per key.
                JSXExpression::ObjectExpression(obj) => {
                    lower_keyed_object(out, "style", obj, source);
                }
                // `style={expr}` → `[style]="expr"`.
                _ => {
                    if let Some(text) = expression_text(&container.expression, source) {
                        push_property_binding(out, "style", text.trim());
                    }
                }
            }
        }
        None => {}
        _ => {}
    }
}

/// Emit one `[prefix.key]="value"` binding per property of `obj` (used for both `class` and
/// `style` object forms). Spread properties (`{...x}`) inside the object are skipped (no per-key
/// equivalent). A shorthand property (`{ active }`) binds the key to itself (`[class.active]="active"`).
fn lower_keyed_object(out: &mut String, prefix: &str, obj: &ObjectExpression, source: &str) {
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(prop) = prop else {
            continue;
        };
        let Some(key) = prop.key.static_name() else {
            continue;
        };
        // The value expression's source text is the binding RHS; for a shorthand the value IS the
        // key identifier, which OXC fills in, so the same span slice works.
        let value = expression_source(&prop.value, source);
        let binding = format!("{prefix}.{key}");
        push_property_binding(out, &binding, value.trim());
    }
}

/// Lower a `{...obj}` spread attribute.
///
/// Angular templates have no first-class object-spread binding, so we lower the spread to a single
/// property binding against the reserved `ngSpreadBindings` input: `[ngSpreadBindings]="obj"`. A
/// host directive/component can consume that object and apply the bindings at runtime. This keeps
/// the spread's source object intact (rather than silently dropping it) and gives a stable,
/// documented attachment point; the chosen name avoids colliding with real DOM attributes.
fn lower_spread(out: &mut String, argument: &Expression, source: &str) {
    let text = expression_source(argument, source);
    push_property_binding(out, "ngSpreadBindings", text.trim());
}

/// Lower a non-structural directive application onto the host element.
///
/// All three syntaxes resolve to the same thing: register the directive class for selectorless
/// auto-import, then emit its primary-input binding on the host so the directive's `@Input` receives
/// the value (or, for a value-less application, emit the bare input attribute so a `[input]`-style
/// directive selector matches and the directive activates):
///   * value-bound (`use:tooltip={x}`, `Tooltip="hi"`, `Tooltip={x}`, bare `tooltip={x}`):
///     a string literal emits `inputname="literal"`; an expression emits `[inputname]="expr"`.
///   * value-less (`use:autofocus`, `<input Autofocus/>`): emits the bare `inputname=""` attribute.
fn lower_directive(
    out: &mut String,
    app: &super::directives::DirectiveApplication,
    attr: &oxc_ast::ast::JSXAttribute,
    source: &str,
) {
    super::directives::register_directive_reference(&app.class_name);

    let input = app
        .input_name
        .clone()
        .unwrap_or_else(|| super::directives::primary_input_name_of(&app.class_name));

    match &attr.value {
        // `Tooltip="hi"` / bare `tooltip="hi"`: a static string sets the input as a literal attr.
        Some(JSXAttributeValue::StringLiteral(s)) => {
            push_string_attr(out, &input, s.value.as_str());
        }
        // `use:tooltip={x}` / `Tooltip={x}`: bind the input to the expression.
        Some(JSXAttributeValue::ExpressionContainer(container)) => {
            if let Some(text) = expression_text(&container.expression, source) {
                push_property_binding(out, &input, text.trim());
            } else {
                push_string_attr(out, &input, "");
            }
        }
        // Value-less (`use:autofocus`, `Autofocus`): the bare input attribute activates the directive.
        _ => {
            push_string_attr(out, &input, "");
        }
    }
}

// ---------------------------------------------------------------------------
// Emit helpers.
// ---------------------------------------------------------------------------

/// `name="value"` (string attribute / void binding).
fn push_string_attr(out: &mut String, name: &str, value: &str) {
    out.push(' ');
    out.push_str(name);
    out.push_str("=\"");
    out.push_str(value);
    out.push('"');
}

/// `[name]="expr"` (Angular property binding).
fn push_property_binding(out: &mut String, name: &str, expr: &str) {
    out.push(' ');
    out.push('[');
    out.push_str(name);
    out.push_str("]=\"");
    out.push_str(expr);
    out.push('"');
}

/// `(event)="handler($event)"` (Angular event binding). When the handler text is already a call
/// (it contains a `(`), it is emitted verbatim; a bare reference (`handler`) is invoked with
/// `$event` so the DOM event reaches the handler.
fn lower_event(out: &mut String, event: &str, handler: &str) {
    out.push(' ');
    out.push('(');
    out.push_str(event);
    out.push_str(")=\"");
    if handler.contains('(') {
        out.push_str(handler);
    } else {
        out.push_str(handler);
        out.push_str("($event)");
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// Name / expression helpers.
// ---------------------------------------------------------------------------

/// The tag name for a JSX element. Component-like names (`<Foo>`) and DOM names (`<div>`) are both
/// emitted verbatim; render3's selectorless binder resolves component refs by class name.
fn element_name(name: &JSXElementName) -> String {
    match name {
        JSXElementName::Identifier(id) => id.name.to_string(),
        JSXElementName::IdentifierReference(id) => id.name.to_string(),
        JSXElementName::NamespacedName(n) => {
            format!("{}:{}", n.namespace.name.as_str(), n.name.name.as_str())
        }
        JSXElementName::MemberExpression(_) => member_expression_placeholder(),
        JSXElementName::ThisExpression(_) => "this".to_string(),
    }
}

/// `<Foo.Bar />`-style member tags have no selectorless analogue; emit a stable `ng-container` so
/// the surrounding template still parses and the children are preserved unwrapped.
fn member_expression_placeholder() -> String {
    "ng-container".to_string()
}

/// The attribute name (left side of a JSX prop).
fn attribute_name(name: &JSXAttributeName) -> String {
    match name {
        JSXAttributeName::Identifier(id) => id.name.to_string(),
        JSXAttributeName::NamespacedName(n) => {
            format!("{}:{}", n.namespace.name.as_str(), n.name.name.as_str())
        }
    }
}

/// Verbatim source text of a JSX expression (the inside of a `{ … }`), or `None` for an empty
/// expression (`{}` / a comment-only container).
fn expression_text(expression: &JSXExpression, source: &str) -> Option<String> {
    if let JSXExpression::EmptyExpression(_) = expression {
        return None;
    }
    let span = oxc_span::GetSpan::span(expression);
    Some(source[span.start as usize..span.end as usize].to_string())
}

/// Verbatim source text of a plain expression (object property values, spread arguments, array
/// elements). Always present (these are never the JSX `EmptyExpression`).
pub(crate) fn expression_source(expression: &Expression, source: &str) -> String {
    let span = oxc_span::GetSpan::span(expression);
    source[span.start as usize..span.end as usize].to_string()
}

/// Whether `name` is a boolean DOM attribute — the set whose mere presence sets the attribute. A
/// JSX boolean-shorthand (`<input disabled />`) on one of these renders as the bare attribute
/// (`disabled=""`); other shorthands lower to a `[attr]="true"` binding instead.
fn is_boolean_attribute(name: &str) -> bool {
    matches!(
        name,
        "disabled"
            | "checked"
            | "readonly"
            | "required"
            | "selected"
            | "multiple"
            | "hidden"
            | "autofocus"
            | "autoplay"
            | "controls"
            | "loop"
            | "muted"
            | "open"
            | "default"
            | "novalidate"
            | "formnovalidate"
            | "ismap"
            | "reversed"
            | "async"
            | "defer"
    )
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use oxc_allocator::Allocator;
    use oxc_ast::ast::{Expression, Statement};
    use oxc_parser::Parser as JsParser;
    use oxc_span::SourceType;

    /// Parse `expr` (a JSX element or fragment expression) and lower it to template HTML, exactly
    /// as the component pipeline does for a `return <JSX/>`.
    fn lower(expr_src: &str) -> String {
        // Wrap in a statement so OXC parses the JSX as an expression; slicing uses the original
        // source spans, so the wrapper offset is irrelevant (spans are absolute into `src`).
        let src = format!("const __x = {expr_src};");
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, &src, SourceType::tsx()).parse();
        assert!(
            ret.errors.is_empty(),
            "parse errors for {expr_src:?}: {:?}",
            ret.errors
        );
        for stmt in &ret.program.body {
            if let Statement::VariableDeclaration(decl) = stmt {
                for d in &decl.declarations {
                    match &d.init {
                        Some(Expression::JSXElement(el)) => return lower_element(el, &src),
                        Some(Expression::JSXFragment(frag)) => return lower_fragment(frag, &src),
                        _ => {}
                    }
                }
            }
        }
        panic!("no JSX expression found in {expr_src:?}");
    }

    #[test]
    fn html_element_passes_through() {
        assert_eq!(lower("<div></div>"), "<div></div>");
        assert_eq!(lower("<input />"), "<input />");
    }

    #[test]
    fn capitalized_component_tag_passes_through_by_name() {
        // Selectorless resolution: the component tag name is preserved verbatim.
        assert_eq!(lower("<MyWidget></MyWidget>"), "<MyWidget></MyWidget>");
        assert_eq!(lower("<MyWidget />"), "<MyWidget />");
    }

    #[test]
    fn fragment_renders_children_unwrapped() {
        let out = lower("<><span>a</span><span>b</span></>");
        assert_eq!(out, "<span>a</span><span>b</span>");
    }

    #[test]
    fn text_and_interpolation_children() {
        // Text is preserved; `{count()}` becomes `{{ count() }}`.
        let out = lower("<div>Hello {count()}</div>");
        assert!(out.contains("Hello"), "text dropped; got {out}");
        assert!(out.contains("{{ count() }}"), "no interpolation; got {out}");
    }

    #[test]
    fn whitespace_only_newline_text_is_dropped() {
        // A newline+indentation between elements is insignificant JSX whitespace.
        let out = lower("<div>\n  <span>x</span>\n</div>");
        assert_eq!(out, "<div><span>x</span></div>");
    }

    #[test]
    fn class_string_passes_through() {
        assert_eq!(lower("<div class=\"a b\"></div>"), "<div class=\"a b\"></div>");
    }

    #[test]
    fn classname_is_treated_as_class() {
        assert_eq!(
            lower("<div className=\"box\"></div>"),
            "<div class=\"box\"></div>"
        );
    }

    #[test]
    fn class_expression_is_property_binding() {
        assert_eq!(
            lower("<div class={cls}></div>"),
            "<div [class]=\"cls\"></div>"
        );
    }

    #[test]
    fn class_object_lowers_per_key() {
        let out = lower("<div class={{ active: isActive, big: size > 2 }}></div>");
        assert!(
            out.contains("[class.active]=\"isActive\""),
            "no active class binding; got {out}"
        );
        assert!(
            out.contains("[class.big]=\"size > 2\""),
            "no big class binding; got {out}"
        );
    }

    #[test]
    fn class_array_joins() {
        let out = lower("<div class={['a', dynamic]}></div>");
        assert!(
            out.contains("[class]=\"['a', dynamic].join(' ')\""),
            "array class not joined; got {out}"
        );
    }

    #[test]
    fn style_object_lowers_per_key() {
        let out = lower("<div style={{ color: c, fontSize: fs }}></div>");
        assert!(
            out.contains("[style.color]=\"c\""),
            "no style.color binding; got {out}"
        );
        assert!(
            out.contains("[style.fontSize]=\"fs\""),
            "no style.fontSize binding; got {out}"
        );
    }

    #[test]
    fn style_string_passes_through() {
        assert_eq!(
            lower("<div style=\"color: red\"></div>"),
            "<div style=\"color: red\"></div>"
        );
    }

    #[test]
    fn prop_expression_is_property_binding() {
        assert_eq!(lower("<foo prop={x}></foo>"), "<foo [prop]=\"x\"></foo>");
    }

    #[test]
    fn string_attribute_passes_through() {
        assert_eq!(
            lower("<a href=\"/x\"></a>"),
            "<a href=\"/x\"></a>"
        );
    }

    #[test]
    fn boolean_shorthand_known_attribute() {
        assert_eq!(lower("<input disabled />"), "<input disabled=\"\" />");
    }

    #[test]
    fn boolean_shorthand_unknown_becomes_true_binding() {
        assert_eq!(lower("<Foo active />"), "<Foo [active]=\"true\" />");
    }

    #[test]
    fn event_handler_lowers_to_angular_event() {
        let out = lower("<button onClick={handle}></button>");
        assert!(
            out.contains("(click)=\"handle($event)\""),
            "no click event; got {out}"
        );
    }

    #[test]
    fn event_handler_call_is_emitted_verbatim() {
        let out = lower("<input onInput={set(value)} />");
        assert!(
            out.contains("(input)=\"set(value)\""),
            "call handler not verbatim; got {out}"
        );
    }

    #[test]
    fn event_name_derivation() {
        assert!(lower("<a onMouseEnter={h}></a>").contains("(mouseenter)="));
        assert!(lower("<a onDblClick={h}></a>").contains("(dblclick)="));
    }

    #[test]
    fn spread_lowers_to_documented_binding() {
        let out = lower("<div {...props}></div>");
        assert!(
            out.contains("[ngSpreadBindings]=\"props\""),
            "spread mapping missing; got {out}"
        );
    }

    #[test]
    fn nested_children_recurse() {
        let out = lower("<div class=\"x\"><span prop={y}>{z}</span></div>");
        assert_eq!(
            out,
            "<div class=\"x\"><span [prop]=\"y\">{{ z }}</span></div>"
        );
    }

    #[test]
    fn combined_div_class_interpolation_and_click() {
        // The headline acceptance case: class string + `{count()}` interpolation + onClick.
        let out = lower("<div class=\"counter\" onClick={inc}>{count()}</div>");
        assert!(out.contains("class=\"counter\""), "no class; got {out}");
        assert!(out.contains("{{ count() }}"), "no interpolation; got {out}");
        assert!(out.contains("(click)=\"inc($event)\""), "no click; got {out}");
    }

    // ----- directives -------------------------------------------------------

    /// Lower with a seeded directive candidate set, returning the markup and the directive class
    /// references collected during lowering.
    fn lower_with(candidates: &[&str], expr_src: &str) -> (String, Vec<String>) {
        let owned: Vec<String> = candidates.iter().map(|s| s.to_string()).collect();
        super::super::directives::begin_pass(&owned);
        let html = lower(expr_src);
        let refs = super::super::directives::take_directive_references();
        (html, refs)
    }

    #[test]
    fn namespace_directive_no_value_applies_directive() {
        // PREFERRED form: `use:autofocus` applies the `Autofocus` directive (value-less). The host
        // carries the bare input attribute so a `[autofocus]` selector matches, and `Autofocus` is
        // collected for auto-import.
        let (html, refs) = lower_with(&[], "<input use:autofocus />");
        assert_eq!(html, "<input autofocus=\"\" />");
        assert_eq!(refs, vec!["Autofocus".to_string()]);
    }

    #[test]
    fn namespace_directive_with_value_binds_input() {
        // PREFERRED form: `use:tooltip={msg}` applies `Tooltip` and binds its `tooltip` input.
        let (html, refs) = lower_with(&[], "<button use:tooltip={msg}>hi</button>");
        assert_eq!(html, "<button [tooltip]=\"msg\">hi</button>");
        assert_eq!(refs, vec!["Tooltip".to_string()]);
    }

    #[test]
    fn capitalized_directive_no_value_applies() {
        // `<input Autofocus/>` applies the `Autofocus` directive.
        let (html, refs) = lower_with(&[], "<input Autofocus />");
        assert_eq!(html, "<input autofocus=\"\" />");
        assert_eq!(refs, vec!["Autofocus".to_string()]);
    }

    #[test]
    fn capitalized_directive_string_value_binds_primary_input() {
        // `<button Tooltip="hi">` binds the lower-camel primary input `tooltip` with the literal.
        let (html, refs) = lower_with(&[], "<button Tooltip=\"hi\">x</button>");
        assert_eq!(html, "<button tooltip=\"hi\">x</button>");
        assert_eq!(refs, vec!["Tooltip".to_string()]);
    }

    #[test]
    fn capitalized_directive_expression_value_binds_primary_input() {
        // `Tooltip={expr}` binds `[tooltip]="expr"`.
        let (html, refs) = lower_with(&[], "<button Tooltip={msg}>x</button>");
        assert_eq!(html, "<button [tooltip]=\"msg\">x</button>");
        assert_eq!(refs, vec!["Tooltip".to_string()]);
    }

    #[test]
    fn bare_lowercase_attribute_applies_known_directive() {
        // The Angular-attribute form: a bare lowercase attribute matching a known imported directive
        // class applies it; `tooltip={msg}` → directive `Tooltip`, input `tooltip`.
        let (html, refs) = lower_with(&["Tooltip"], "<span tooltip={msg}>x</span>");
        assert_eq!(html, "<span [tooltip]=\"msg\">x</span>");
        assert_eq!(refs, vec!["Tooltip".to_string()]);
    }

    #[test]
    fn bare_lowercase_attribute_unknown_is_plain_dom_attribute() {
        // Without a matching import, a lowercase attribute is an ordinary DOM property binding and is
        // NOT recorded as a directive reference.
        let (html, refs) = lower_with(&[], "<span tooltip={msg}>x</span>");
        assert_eq!(html, "<span [tooltip]=\"msg\">x</span>");
        assert!(refs.is_empty(), "unexpected directive refs: {refs:?}");
    }

    #[test]
    fn structural_directive_lowers_to_ng_template() {
        // The structural directive (`structural:highlight={c}`, the JSX spelling of `*highlight`)
        // wraps the host in an `<ng-template>` carrying the structural binding, as `*ngIf` desugars.
        let (html, refs) = lower_with(&["Highlight"], "<div structural:highlight={c}>x</div>");
        assert_eq!(
            html,
            "<ng-template [highlight]=\"c\"><div>x</div></ng-template>"
        );
        assert_eq!(refs, vec!["Highlight".to_string()]);
    }

    #[test]
    fn structural_directive_no_value_lowers_to_ng_template() {
        let (html, refs) = lower_with(&["Highlight"], "<div structural:highlight>x</div>");
        assert_eq!(
            html,
            "<ng-template highlight=\"\"><div>x</div></ng-template>"
        );
        assert_eq!(refs, vec!["Highlight".to_string()]);
    }

    #[test]
    fn directive_coexists_with_dom_attributes_and_events() {
        // A directive application sits alongside ordinary attributes/events on the same element.
        let (html, refs) = lower_with(
            &[],
            "<button class=\"b\" use:tooltip={msg} onClick={go}>x</button>",
        );
        assert!(html.contains("class=\"b\""), "no class; got {html}");
        assert!(html.contains("[tooltip]=\"msg\""), "no tooltip input; got {html}");
        assert!(html.contains("(click)=\"go($event)\""), "no click; got {html}");
        assert_eq!(refs, vec!["Tooltip".to_string()]);
    }
}
