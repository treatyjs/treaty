//! End-to-end direct-to-Ivy pipeline glue.
//!
//! Wires together the already-ported modules into a single entry point that takes a component
//! template (HTML string) + selector + class name and produces the emitted JS for the
//! `ɵɵdefineComponent({...})` expression, including a *real* template instruction function
//! (`ɵɵelement`/`ɵɵtext`/`ɵɵtextInterpolate1`/…) produced by the classic
//! [`crate::view::template::TemplateDefinitionBuilder`].
//!
//! Pipeline:
//! ```text
//! ml_parser::parse(html)
//!   -> html_ast_to_render3_ast            (HTML AST -> r3_ast Node tree)
//!   -> R3ComponentMetadata (minimal)      (name, selector, standalone, template nodes)
//!   -> compile_component_from_metadata    (assembles ɵɵdefineComponent fields)
//!        \- RealTemplateBuilder           (drives TemplateDefinitionBuilder for the template fn)
//!   -> emit_expression                    (output_ast -> JS string via oxc_codegen)
//! ```

use crate::factory::{R3CompiledExpression, R3Reference};
use crate::output::emitter::emit_expression;
use crate::output_ast::{self as o, Expr, ParseSourceSpan};
use crate::template::template_transform::{
    html_ast_to_render3_ast, BindingParser, Render3ParseOptions,
};
use crate::view::compiler::{
    compile_component_from_metadata, ChangeDetection, ChangeDetectionStrategy, ComponentTemplate,
    DeclarationListEmitMode, Deps, Lifecycle, OrderedMap, R3ComponentDeferMetadata,
    R3ComponentMetadata, R3DirectiveMetadata, R3HostMetadata, R3TemplateDependency,
    R3TemplateDependencyMetadata, StubHostBindingsBuilder, TemplateBuilder, TemplateBuilderResult,
    ViewEncapsulation,
};
use crate::view::template::{TemplateCompilationInput, TemplateDefinitionBuilder};

/// The result of compiling one component to its Ivy definition.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledComponent {
    /// The emitted JS source for the `ɵɵdefineComponent({...})` expression.
    pub code: String,
    /// Diagnostics gathered while parsing the template (binding/transform errors).
    pub errors: Vec<String>,
}

/// A real [`TemplateBuilder`] that drives the classic [`TemplateDefinitionBuilder`] over the
/// component's r3_ast template nodes, producing the actual creation/update instruction stream
/// (instead of the empty-bodied [`crate::view::compiler::StubTemplateBuilder`]).
#[derive(Debug, Default)]
pub struct RealTemplateBuilder;

impl TemplateBuilder for RealTemplateBuilder {
    fn build<D: R3TemplateDependency>(
        &mut self,
        meta: &R3ComponentMetadata<D>,
        _all_deferrable_deps_fn: Option<&Expr>,
    ) -> TemplateBuilderResult {
        let name = format!("{}_Template", meta.base.name);
        let input = TemplateCompilationInput::new(name, meta.template.nodes.clone());
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let template_fn = builder.build_template_function(&input);

        // `decls` = number of allocated data slots. NOTE(port): the classic TDB also counts
        // pipe/projection slots; this minimal builder allocates one slot per element/text node.
        let decls = builder.data_index() as u32;
        // `vars` = the binding-slot count the builder accumulated while emitting property /
        // interpolation bindings (`allocateBindingSlots`), matching Angular's `calculateBindingSlots`.
        let vars = builder.vars() as u32;

        let consts = builder.const_pool().entries().to_vec();

        TemplateBuilderResult {
            template_fn,
            decls,
            vars,
            consts,
            // NOTE(port): const initializers (e.g. i18n message vars) are not produced by the
            // standalone builder yet.
            consts_initializers: Vec::new(),
            // NOTE(port): ngContentSelectors come from `<ng-content>` projection slots, not yet
            // emitted by the standalone builder.
            content_selectors: None,
        }
    }
}

/// A "class reference" expression standing in for the component class symbol. NOTE(port): the real
/// compiler threads a resolved `o.WrappedNodeExpr`/import here; a bare identifier read of the class
/// name keeps the emitted definition well-formed.
fn class_ref(class_name: &str) -> R3Reference {
    R3Reference {
        value: o::variable(class_name, None),
        ty: o::variable(class_name, None),
    }
}

/// Build a minimal-but-faithful [`R3DirectiveMetadata`] base for a standalone component.
fn base_metadata(selector: &str, class_name: &str) -> R3DirectiveMetadata {
    R3DirectiveMetadata {
        name: class_name.to_string(),
        ty: class_ref(class_name),
        type_argument_count: 0,
        // NOTE(port): the type source span is only used for diagnostics/`.d.ts`; an empty span is
        // faithful for emission.
        type_source_span: ParseSourceSpan::new(0, 0),
        deps: Deps::None,
        selector: Some(selector.to_string()),
        queries: Vec::new(),
        view_queries: Vec::new(),
        host: R3HostMetadata::default(),
        lifecycle: Lifecycle::default(),
        inputs: OrderedMap::new(),
        outputs: OrderedMap::new(),
        uses_inheritance: false,
        control_create: None,
        export_as: None,
        providers: None,
        is_standalone: true,
        is_signal: false,
        host_directives: None,
        legacy_optional_chaining: false,
    }
}

/// Compile a single standalone component from its template HTML, selector and class name.
///
/// Returns the emitted JS for the `ɵɵdefineComponent({...})` expression (with a real template
/// instruction function) plus any template-parse diagnostics.
pub fn compile_component(
    template_html: &str,
    selector: &str,
    class_name: &str,
) -> CompiledComponent {
    let mut errors: Vec<String> = Vec::new();

    // 1. HTML AST.
    let parse_result = crate::ml_parser::parse(template_html, "template.html");
    for e in &parse_result.errors {
        errors.push(e.msg.clone());
    }

    // 2. HTML AST -> r3_ast Node tree.
    let mut binding_parser = BindingParser::new();
    let r3 = html_ast_to_render3_ast(
        &parse_result.root_nodes,
        &mut binding_parser,
        Render3ParseOptions::default(),
    );
    for e in &r3.errors {
        errors.push(e.msg.clone());
    }

    // 3. Build minimal component metadata.
    let mut meta: R3ComponentMetadata<R3TemplateDependencyMetadata> = R3ComponentMetadata {
        base: base_metadata(selector, class_name),
        template: ComponentTemplate {
            nodes: r3.nodes,
            ng_content_selectors: r3.ng_content_selectors,
            preserve_whitespaces: None,
        },
        declarations: Vec::new(),
        // NOTE(port): no `@defer` blocks in this minimal pipeline.
        defer: R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: None,
        },
        declaration_list_emit_mode: DeclarationListEmitMode::Direct,
        styles: Vec::new(),
        external_styles: None,
        // NOTE(port): no styles -> compile_component_from_metadata normalizes Emulated -> None.
        encapsulation: ViewEncapsulation::Emulated,
        animations: None,
        view_providers: None,
        relative_context_file_path: String::new(),
        i18n_use_external_ids: false,
        // Default strategy; not emitted (Default differs from the implicit OnPush default, so it
        // would be emitted — use OnPush to keep output minimal). NOTE(port): real default is
        // determined by the decorator.
        change_detection: Some(ChangeDetection::Strategy(ChangeDetectionStrategy::OnPush)),
        relative_template_path: None,
        has_directive_dependencies: false,
        raw_imports: None,
        foreign_imports: None,
    };

    // 4. Assemble the definition (real template builder + stub host bindings).
    let mut template_builder = RealTemplateBuilder;
    let mut host_builder = StubHostBindingsBuilder;
    let mut pool_statements = Vec::new();
    let compiled: R3CompiledExpression = compile_component_from_metadata(
        &mut meta,
        &mut template_builder,
        &mut host_builder,
        &mut pool_statements,
    );

    // 5. Emit the definition expression to JS.
    let code = emit_expression(&compiled.expression);

    CompiledComponent { code, errors }
}

// ---------------------------------------------------------------------------
// Tests.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_div_with_interpolation_end_to_end() {
        let out = compile_component("<div>{{name}}</div>", "app-hello", "HelloComponent");

        // No fatal parse errors for a trivial template.
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        let code = &out.code;
        assert!(code.contains("\u{0275}\u{0275}defineComponent"), "got: {code}");
        assert!(code.contains("HelloComponent"), "got: {code}");
        // A real template instruction function.
        assert!(code.contains("HelloComponent_Template"), "got: {code}");
        // Real instructions (not the empty stub body).
        assert!(code.contains("\u{0275}\u{0275}text"), "got: {code}");
        // `{{name}}` (single expr, empty affixes) faithfully collapses to the bare
        // `ɵɵtextInterpolate(ctx.name)` (Angular's `collateInterpolationArgs` + arity 0).
        assert!(code.contains("\u{0275}\u{0275}textInterpolate"), "got: {code}");
        // The bound expression resolves against ctx.
        assert!(code.contains("ctx.name"), "got: {code}");

        // Print the exact emitted JS for the report.
        println!("=== <div>{{{{name}}}}</div> ===\n{code}");
    }

    #[test]
    fn compiles_static_button_end_to_end() {
        let out = compile_component("<button>Hi</button>", "app-btn", "BtnComponent");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        let code = &out.code;
        assert!(code.contains("\u{0275}\u{0275}defineComponent"), "got: {code}");
        assert!(code.contains("BtnComponent"), "got: {code}");
        assert!(code.contains("BtnComponent_Template"), "got: {code}");
        // Static text inside an element.
        assert!(code.contains("\u{0275}\u{0275}text"), "got: {code}");
        assert!(code.contains("\"Hi\""), "got: {code}");

        println!("=== <button>Hi</button> ===\n{code}");
    }

    /// Robustness sweep: a variety of template shapes must compile without panicking
    /// and always yield a `ɵɵdefineComponent`. Guards the transform/binder/view layers
    /// against crashes on shapes the per-module tests didn't cover. (Instruction-level
    /// fidelity for bindings / control-flow is a separate NOTE(port) concern — here we
    /// only assert "does not panic" + a valid definition is produced.)
    #[test]
    fn compile_component_does_not_panic_on_varied_templates() {
        let cases: &[&str] = &[
            "",
            "hello",
            "<div></div>",
            "<input>",
            "<div><span>hi</span></div>",
            "<p>{{a}} and {{b}}</p>",
            "<div>{{a + b}}</div>",
            "<div class=\"box\" id=\"x\">y</div>",
            "<div [id]=\"x\">y</div>",
            "<button (click)=\"f()\">go</button>",
            "<div [class.active]=\"on\">z</div>",
            "<img src=\"a.png\" />",
            "<ul><li>{{i}}</li></ul>",
            "@if (cond) { <div>a</div> } @else { <div>b</div> }",
            "@for (item of items; track item) { <li>{{item}}</li> }",
            "@switch (k) { @case (1) { <p>one</p> } @default { <p>n</p> } }",
            "<div #ref>{{ref.value}}</div>",
        ];
        for (i, tpl) in cases.iter().enumerate() {
            let out = compile_component(tpl, "app-x", "XComponent");
            assert!(
                out.code.contains("\u{0275}\u{0275}defineComponent"),
                "case {i} ({tpl:?}) produced no defineComponent; errors={:?}\ncode={}",
                out.errors,
                out.code
            );
        }
    }
}
