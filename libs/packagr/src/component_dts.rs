//! Angular **component** declaration (`.d.ts`) synthesis.
//!
//! The compiled output of a Treaty/Angular component is machine-generated
//! JavaScript: a plain `function Button() { … }` carrying static `ɵfac`/`ɵcmp`
//! fields assigned from `ɵɵdefineComponent({ … })`. The component *function* has
//! no return-type annotation, so running `oxc_isolated_declarations` over it
//! fails with `TS9007` (an explicit return type is required under
//! `--isolatedDeclarations`).
//!
//! Instead of declaring the lowered function, we reconstruct the *Angular
//! component class* the author wrote — exactly the `.d.ts` `ngc`/`ng-packagr`
//! would emit for an Ivy component:
//!
//! ```ts
//! import * as i0 from "@angular/core";
//! export declare class Button {
//!     variant: import("@angular/core").InputSignal<'default' | 'outline'>;
//!     // …
//!     static ɵfac: i0.ɵɵFactoryDeclaration<Button, never>;
//!     static ɵcmp: i0.ɵɵComponentDeclaration<Button, "button", never, {
//!         "variant": { "alias": "variant"; "required": false; "isSignal": true };
//!     }, {}, never, never, true, never>;
//! }
//! export default Button;
//! ```
//!
//! The class name, public selector, and the input set are read straight from the
//! emitted `ɵɵdefineComponent` metadata; each input's *type* is recovered from
//! the explicit `input<T>(…)` type argument in the component function body, and
//! otherwise inferred from the `input(default)` argument (`boolean`/`string`/
//! `number`), falling back to `unknown`. The result is valid, useful, and never
//! trips isolated-declarations.

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, Expression, ObjectPropertyKind, PropertyKey, Statement,
};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};

/// One reconstructed signal input on the component class.
struct ComponentInput {
    /// Property / public name (e.g. `variant`).
    name: String,
    /// Public binding alias (the second element of the `ɵcmp` input tuple).
    alias: String,
    /// The TypeScript type carried by the `InputSignal<…>` (e.g. `boolean`).
    ty: String,
    /// Whether the input is required (no default / `input.required`).
    required: bool,
}

/// A component reconstructed from compiled Ivy ESM.
struct ComponentModel {
    /// The component class / function name (e.g. `Button`).
    name: String,
    /// The public component selector (e.g. `"button"`); `never` when absent.
    selector: Option<String>,
    /// The component's signal inputs, in declaration order.
    inputs: Vec<ComponentInput>,
    /// Whether the module default-exports the component.
    default_exported: bool,
}

/// Synthesize an Angular component-class `.d.ts` from compiled Ivy ESM.
///
/// Returns `None` when `compiled` is not recognizably a single Ivy component
/// (no `ɵɵdefineComponent`), so the caller can fall back to its generic
/// export-surface synthesizer.
pub fn synthesize_component_dts(compiled: &str) -> Option<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let parsed = Parser::new(&allocator, compiled, source_type).parse();
    if !parsed.errors.is_empty() {
        return None;
    }

    let model = extract_component(&parsed.program.body, compiled)?;
    Some(render_component_dts(&model))
}

/// Walk the module body and reconstruct the component model, or `None` if the
/// module is not a single Ivy component.
fn extract_component(body: &[Statement<'_>], source: &str) -> Option<ComponentModel> {
    // 1. Find the `<Name>.ɵcmp = <expr>` assignment and pull the component name +
    //    the `ɵɵdefineComponent({ … })` metadata object from it.
    let mut name: Option<String> = None;
    let mut selector: Option<String> = None;
    let mut input_specs: Vec<(String, String)> = Vec::new(); // (name, alias)

    for stmt in body {
        let Statement::ExpressionStatement(expr_stmt) = stmt else {
            continue;
        };
        let Expression::AssignmentExpression(assign) = &expr_stmt.expression else {
            continue;
        };
        // LHS must be `<Ident>.ɵcmp`.
        let Some((obj_name, prop)) = assignment_static_member(assign) else {
            continue;
        };
        if prop != "\u{0275}cmp" {
            continue;
        }
        // RHS must be a `…ɵɵdefineComponent({ … })` call.
        let Expression::CallExpression(call) = &assign.right else {
            continue;
        };
        if !callee_is_define_component(&call.callee) {
            continue;
        }
        let Some(Argument::ObjectExpression(meta)) = call.arguments.first() else {
            continue;
        };

        name = Some(obj_name);
        for prop in &meta.properties {
            let ObjectPropertyKind::ObjectProperty(p) = prop else {
                continue;
            };
            match property_name(&p.key).as_deref() {
                Some("selectors") => {
                    selector = first_selector(&p.value);
                }
                Some("inputs") => {
                    input_specs = read_inputs(&p.value);
                }
                _ => {}
            }
        }
        break;
    }

    let name = name?;

    // 2. Recover each input's TYPE from the component function body's
    //    `const <name> = input<T>(default)` declarations.
    let type_map = input_types_from_function(body, &name, source);

    let inputs = input_specs
        .into_iter()
        .map(|(input_name, alias)| {
            let (ty, required) = type_map
                .iter()
                .find(|(n, _, _)| *n == input_name)
                .map(|(_, ty, req)| (ty.clone(), *req))
                .unwrap_or_else(|| ("unknown".to_string(), false));
            ComponentInput {
                name: input_name,
                alias,
                ty,
                required,
            }
        })
        .collect();

    // 3. Does the module default-export the component?
    let default_exported = body.iter().any(|s| {
        matches!(s, Statement::ExportDefaultDeclaration(_))
    });

    Some(ComponentModel {
        name,
        selector,
        inputs,
        default_exported,
    })
}

/// `<Ident>.<prop>` on the LHS of an assignment → `(ident, prop)`.
///
/// `AssignmentTarget` inherits `MemberExpression`'s variants, so a
/// `Button.ɵcmp = …` target surfaces directly as `StaticMemberExpression`.
fn assignment_static_member(
    assign: &oxc_ast::ast::AssignmentExpression<'_>,
) -> Option<(String, String)> {
    use oxc_ast::ast::AssignmentTarget;
    let AssignmentTarget::StaticMemberExpression(member) = &assign.left else {
        return None;
    };
    let Expression::Identifier(obj) = &member.object else {
        return None;
    };
    Some((obj.name.to_string(), member.property.name.to_string()))
}

/// Is `callee` a reference to `ɵɵdefineComponent` (bare or `i0.ɵɵdefineComponent`)?
fn callee_is_define_component(callee: &Expression<'_>) -> bool {
    match callee {
        Expression::Identifier(id) => id.name == "\u{0275}\u{0275}defineComponent",
        Expression::StaticMemberExpression(member) => {
            member.property.name == "\u{0275}\u{0275}defineComponent"
        }
        _ => false,
    }
}

/// The string form of an object-property key (identifier or string literal).
fn property_name(key: &PropertyKey<'_>) -> Option<String> {
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
        PropertyKey::StringLiteral(lit) => Some(lit.value.to_string()),
        _ => None,
    }
}

/// Pull the first public selector string from a `selectors: [[…], …]` value.
///
/// Treaty emits `selectors: [["button"], ["Button"]]`; the first non-empty
/// string literal is the public element selector.
fn first_selector(value: &Expression<'_>) -> Option<String> {
    let Expression::ArrayExpression(outer) = value else {
        return None;
    };
    for group in &outer.elements {
        if let oxc_ast::ast::ArrayExpressionElement::ArrayExpression(inner) = group {
            for el in &inner.elements {
                if let oxc_ast::ast::ArrayExpressionElement::StringLiteral(s) = el {
                    let v = s.value.to_string();
                    if !v.is_empty() {
                        return Some(v);
                    }
                }
            }
        }
    }
    None
}

/// Read the `inputs: { name: [flags, "alias"], … }` metadata into `(name, alias)`
/// pairs, preserving declaration order.
fn read_inputs(value: &Expression<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Expression::ObjectExpression(obj) = value else {
        return out;
    };
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            continue;
        };
        let Some(name) = property_name(&p.key) else {
            continue;
        };
        // The value is `[flags, "alias"]` (modern) or `"alias"` (legacy). Read
        // the alias where present, defaulting to the property name.
        let alias = match &p.value {
            Expression::ArrayExpression(arr) => arr
                .elements
                .iter()
                .find_map(|el| match el {
                    oxc_ast::ast::ArrayExpressionElement::StringLiteral(s) => {
                        Some(s.value.to_string())
                    }
                    _ => None,
                })
                .unwrap_or_else(|| name.clone()),
            Expression::StringLiteral(s) => s.value.to_string(),
            _ => name.clone(),
        };
        out.push((name, alias));
    }
    out
}

/// Recover `(input_name, type, required)` for each `const <name> = input<…>(…)`
/// declared inside the component function `fn_name`.
fn input_types_from_function(
    body: &[Statement<'_>],
    fn_name: &str,
    source: &str,
) -> Vec<(String, String, bool)> {
    let mut out = Vec::new();
    for stmt in body {
        let func = match stmt {
            Statement::FunctionDeclaration(f) => f.as_ref(),
            _ => continue,
        };
        let is_target = func.id.as_ref().is_some_and(|id| id.name == fn_name);
        if !is_target {
            continue;
        }
        let Some(fn_body) = &func.body else { continue };
        for inner in &fn_body.statements {
            let Statement::VariableDeclaration(var) = inner else {
                continue;
            };
            for decl in &var.declarations {
                let Some(binding) = decl.id.get_identifier_name() else {
                    continue;
                };
                let Some(Expression::CallExpression(call)) = &decl.init else {
                    continue;
                };
                let Some(kind) = input_call_kind(&call.callee) else {
                    continue;
                };
                let required = kind == InputKind::Required;
                let ty = input_type(call, source);
                out.push((binding.to_string(), ty, required));
            }
        }
        break;
    }
    out
}

#[derive(PartialEq, Eq)]
enum InputKind {
    Plain,
    Required,
}

/// Recognize `input(…)` / `input.required(…)` callees.
fn input_call_kind(callee: &Expression<'_>) -> Option<InputKind> {
    match callee {
        Expression::Identifier(id) if id.name == "input" => Some(InputKind::Plain),
        Expression::StaticMemberExpression(member) => {
            let Expression::Identifier(obj) = &member.object else {
                return None;
            };
            if obj.name == "input" && member.property.name == "required" {
                Some(InputKind::Required)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolve the `InputSignal<T>` element type for an `input<T>(default)` call.
///
/// Prefers the explicit `<T>` type argument (sliced verbatim from the source so
/// union/string-literal types survive). Otherwise infers from the default
/// argument's literal kind (`boolean`/`string`/`number`); falls back to
/// `unknown`. `as <Local>` casts are NOT used — the local alias is out of scope
/// in the emitted `.d.ts`.
fn input_type(call: &oxc_ast::ast::CallExpression<'_>, source: &str) -> String {
    if let Some(args) = &call.type_arguments
        && let Some(first) = args.params.first()
    {
        let text = first.span().source_text(source).trim();
        if !text.is_empty() {
            return text.to_string();
        }
    }

    match call.arguments.first() {
        Some(Argument::BooleanLiteral(_)) => "boolean".to_string(),
        Some(Argument::NumericLiteral(_)) => "number".to_string(),
        Some(Argument::StringLiteral(_)) => "string".to_string(),
        Some(Argument::TemplateLiteral(_)) => "string".to_string(),
        // `input('x' as Foo)`: the default is a cast to a LOCAL type that is not
        // in scope in the .d.ts. Recover the literal's primitive kind instead.
        Some(Argument::TSAsExpression(as_expr)) => match &as_expr.expression {
            Expression::BooleanLiteral(_) => "boolean".to_string(),
            Expression::NumericLiteral(_) => "number".to_string(),
            Expression::StringLiteral(_) => "string".to_string(),
            Expression::TemplateLiteral(_) => "string".to_string(),
            _ => "unknown".to_string(),
        },
        _ => "unknown".to_string(),
    }
}

/// Render the reconstructed component as a `.d.ts` module.
fn render_component_dts(model: &ComponentModel) -> String {
    let core = "@angular/core";
    let name = &model.name;
    let mut out = String::new();
    out.push_str("import * as i0 from \"@angular/core\";\n");
    out.push_str(&format!("export declare class {name} {{\n"));

    // Signal input properties.
    for input in &model.inputs {
        out.push_str(&format!(
            "    {}: import(\"{core}\").InputSignal<{}>;\n",
            input.name, input.ty
        ));
    }

    // ɵfac
    out.push_str(&format!(
        "    static \u{0275}fac: i0.\u{0275}\u{0275}FactoryDeclaration<{name}, never>;\n"
    ));

    // ɵcmp — the inputs map mirrors ngc's ComponentDeclaration shape.
    let inputs_type = if model.inputs.is_empty() {
        "{}".to_string()
    } else {
        let entries: Vec<String> = model
            .inputs
            .iter()
            .map(|i| {
                format!(
                    "\"{}\": {{ \"alias\": \"{}\"; \"required\": {}; \"isSignal\": true; }}",
                    i.name, i.alias, i.required
                )
            })
            .collect();
        format!("{{ {} }}", entries.join("; "))
    };
    let selector_ty = match &model.selector {
        Some(sel) => format!("\"{sel}\""),
        None => "never".to_string(),
    };
    out.push_str(&format!(
        "    static \u{0275}cmp: i0.\u{0275}\u{0275}ComponentDeclaration<{name}, {selector_ty}, never, {inputs_type}, {{}}, never, never, true, never>;\n"
    ));

    out.push_str("}\n");
    if model.default_exported {
        out.push_str(&format!("export default {name};\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUTTON: &str = r#"
import * as i0 from "@angular/core";
import { input, computed } from '@angular/core'
function Button() {
    const variant = input<'default' | 'outline' | 'ghost' | 'destructive'>('default')
    const size = input<'sm' | 'md' | 'lg'>('md')
    const disabled = input(false)
    const label = input<string>('')
    const className = computed(() => `btn`)
    return { variant, size, disabled, label, className };
}
Button.ɵfac = function Button_Factory(t) { return (t || Button)(); };
Button.ɵcmp = i0.ɵɵdefineComponent({
    type: Button,
    selectors: [["button"], ["Button"]],
    inputs: { variant: [1, "variant"], size: [1, "size"], disabled: [1, "disabled"], label: [1, "label"] },
    signals: true,
    template: function Button_Template(rf, ctx) {}
});
export default Button;
"#;

    #[test]
    fn derives_typed_component_class() {
        let dts = synthesize_component_dts(BUTTON).expect("should recognize the component");
        assert!(dts.contains("export declare class Button {"));
        assert!(dts.contains(
            "variant: import(\"@angular/core\").InputSignal<'default' | 'outline' | 'ghost' | 'destructive'>;"
        ));
        assert!(dts.contains("disabled: import(\"@angular/core\").InputSignal<boolean>;"));
        assert!(dts.contains("label: import(\"@angular/core\").InputSignal<string>;"));
        assert!(dts.contains("static \u{0275}fac: i0.\u{0275}\u{0275}FactoryDeclaration<Button, never>;"));
        assert!(dts.contains("static \u{0275}cmp: i0.\u{0275}\u{0275}ComponentDeclaration<Button, \"button\", never,"));
        assert!(dts.contains("\"variant\": { \"alias\": \"variant\"; \"required\": false; \"isSignal\": true; }"));
        assert!(dts.contains("export default Button;"));
        // `className` is a computed, NOT an input → must not appear as a class member.
        assert!(!dts.contains("className:"));
    }

    #[test]
    fn returns_none_for_non_component() {
        assert!(synthesize_component_dts("export const x = 1;").is_none());
        assert!(synthesize_component_dts("export { default as A } from './a';").is_none());
    }

    #[test]
    fn infers_input_without_type_arg() {
        let src = r#"
import * as i0 from "@angular/core";
function Card() {
    const title = input();
    const description = input();
    return { title, description };
}
Card.ɵcmp = i0.ɵɵdefineComponent({
    type: Card,
    selectors: [["card"], ["Card"]],
    inputs: { title: [1, "title"], description: [1, "description"] }
});
export default Card;
"#;
        let dts = synthesize_component_dts(src).unwrap();
        // No type arg + no default → `unknown` (sound, re-parseable).
        assert!(dts.contains("title: import(\"@angular/core\").InputSignal<unknown>;"));
        assert!(dts.contains("export declare class Card {"));
    }

    #[test]
    fn as_cast_default_falls_back_to_primitive() {
        let src = r#"
import * as i0 from "@angular/core";
function Alert() {
    type AlertType = 'info' | 'error';
    const type = input('info' as AlertType);
    return { type };
}
Alert.ɵcmp = i0.ɵɵdefineComponent({
    type: Alert,
    selectors: [["alert"], ["Alert"]],
    inputs: { type: [1, "type"] }
});
export default Alert;
"#;
        let dts = synthesize_component_dts(src).unwrap();
        // The local `AlertType` is out of scope → we emit the primitive `string`.
        assert!(dts.contains("type: import(\"@angular/core\").InputSignal<string>;"));
        assert!(!dts.contains("AlertType"));
    }
}
