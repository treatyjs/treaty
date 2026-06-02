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

use std::cell::RefCell;

use crate::output::emitter::{emit_expression, emit_statements};
use crate::output_ast::{
    self as o, ArrowBody, Expr, ExprKind, ImportUrl, LiteralMapEntry, ParseSourceSpan, Stmt,
    StmtKind, WrappedNodeHandle,
};
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
use crate::util::{R3CompiledExpression, R3Reference};

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
        deferred_deps: &std::collections::HashMap<(u32, u32), Expr>,
    ) -> TemplateBuilderResult {
        let name = format!("{}_Template", meta.base.name);
        // Angular selects the `DomOnly` instruction family (`ɵɵdomElement*`/`ɵɵdomListener`/
        // `ɵɵdomProperty`/`ɵɵdomTemplate`) iff the component `isStandalone && !hasDirectiveDependencies`
        // (`render3/view/compiler.ts`), otherwise the classic `Full` family (`ɵɵelement*`/`ɵɵlistener`/
        // `ɵɵproperty`/`ɵɵtemplate`). Thread that decision into the view builder.
        let dom_only = meta.base.is_standalone && !meta.has_directive_dependencies;
        let input = TemplateCompilationInput::new(name, meta.template.nodes.clone())
            .with_dom_only(dom_only)
            .with_deferred_deps(deferred_deps.clone());
        let mut builder = TemplateDefinitionBuilder::new(&input);
        let template_fn = builder.build_template_function(&input);

        // `decls` = number of allocated data slots after the full walk. `data_index()` already
        // includes the slots the classic TDB allocates for elements/text, projection anchors
        // (`<ng-content>`) and pipes (`finalize_pipes` extends `data_index`), matching Angular's
        // `getConstCount`/data allocation.
        let decls = builder.data_index() as u32;
        // `vars` = the binding-slot count the builder accumulated while emitting property /
        // interpolation bindings (`allocateBindingSlots`), matching Angular's `calculateBindingSlots`.
        let vars = builder.vars() as u32;

        let consts = builder.const_pool().entries().to_vec();

        // `ngContentSelectors` — the component-level projection selector list (Angular's
        // `compileComponentFromMetadata` emits `getConstLiteral(literalArr(ngContentSelectors),
        // /*forceShared*/ true)` when any `<ng-content>` slot exists). The selectors were collected by
        // the transform into `meta.template.ng_content_selectors` (`*` for the catch-all default
        // slot). Angular HOISTS this list into the shared constant pool, so the golden emits
        // `ngContentSelectors: $cN$` with a top-level `const $cN$ = [...]` — NOT an inline array.
        // Intern it AFTER `build_template_function` (above) so the `ɵɵprojectionDef` selector array
        // (`$c0$`) is allocated first and the selector list takes the next ordinal (`$c1$`), matching
        // the goldens; the hoisted declaration is then surfaced below via `hoisted_functions()`.
        let content_selectors =
            builder.intern_content_selectors(&meta.template.ng_content_selectors);

        TemplateBuilderResult {
            template_fn,
            decls,
            vars,
            consts,
            // i18n const-pool initializers: when the template carries an i18n block, each message is
            // collected into the const array as a `$i18n_n$` read-var whose value is assigned lazily
            // in the `consts: () => { …; return [...]; }` arrow body (the closure-mode
            // `let $i18n_n$; if (ngI18nClosureMode) { … goog.getMsg … } else { … $localize … }` form).
            // `TemplateDefinitionBuilder::intern_i18n_message` records those statements on the const
            // pool; surface them here so `compile_component_from_metadata` emits the arrow wrapper.
            consts_initializers: builder.const_pool().initializers().to_vec(),
            content_selectors,
            // Hoisted nested-view functions (`@if`/`@for`/`@switch`/`@defer` branch + loop bodies,
            // projection fallbacks, `ng-template` bodies) — surfaced as top-level sibling
            // declarations on `ConstantPool.statements`, emitted before the `ɵɵdefineComponent`
            // call exactly as Angular does (the root view body never inlines them).
            pool_statements: builder.hoisted_functions().to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// WrappedNodeExpr handle registry.
//
// In the real ngtsc pipeline an `R3Reference` to the component *class* is an `o.WrappedNodeExpr`
// wrapping the resolved host (TypeScript) AST node for the class symbol; the emitter prints that
// node as the class identifier. Our IR mirrors this with an opaque [`WrappedNodeHandle`] indexing a
// side table of resolved host expressions ([`WrappedNode`](ExprKind::WrappedNode) — see
// `output_ast.rs`).
//
// `class_ref` therefore registers the class identifier expression in this thread-local side table
// and returns a `WrappedNode(handle)` reference (faithful to `new o.WrappedNodeExpr(classNode)`).
// Because the JS emitter has no view of this side table, [`resolve_wrapped_nodes`] substitutes each
// handle back to its registered expression just before emission, so the wrapped class node prints
// as the class identifier exactly as ngtsc emits it.
// ---------------------------------------------------------------------------

thread_local! {
    /// Side table of host AST node expressions referenced by [`WrappedNodeHandle`], in allocation
    /// order. Indexed by `handle.0`. Mirrors ngtsc's `WrappedNodeExpr.node` storage.
    static WRAPPED_NODES: RefCell<Vec<Expr>> = const { RefCell::new(Vec::new()) };
}

/// Register `expr` as a wrapped host node and return its `WrappedNode(handle)` reference.
fn wrap_node(expr: Expr) -> Expr {
    let handle = WRAPPED_NODES.with(|table| {
        let mut table = table.borrow_mut();
        let index = table.len() as u32;
        table.push(expr);
        WrappedNodeHandle(index)
    });
    Expr::bare(ExprKind::WrappedNode(handle))
}

/// Look up the registered host expression for a [`WrappedNodeHandle`].
fn resolved_wrapped_node(handle: WrappedNodeHandle) -> Option<Expr> {
    WRAPPED_NODES.with(|table| table.borrow().get(handle.0 as usize).cloned())
}

/// Clear the wrapped-node side table (called at the start of each component compilation so handles
/// are stable and the table does not grow unboundedly across calls).
fn reset_wrapped_nodes() {
    WRAPPED_NODES.with(|table| table.borrow_mut().clear());
}

/// The component class reference: an `o.WrappedNodeExpr` wrapping the class identifier node (used
/// for both the runtime `value` and the `.d.ts` `type`), faithful to ngtsc's `R3Reference`.
fn class_ref(class_name: &str) -> R3Reference {
    R3Reference {
        value: wrap_node(o::variable(class_name, None)),
        ty: wrap_node(o::variable(class_name, None)),
    }
}

/// Convert a kebab-case / camelCase element tag (`<foo-bar>`, `<fooBar>`) to its PascalCase class
/// name (`FooBar`), so a selectorless usage written with the HTML-friendly tag spelling can be
/// matched back to an imported class identifier. A tag that is already PascalCase round-trips
/// unchanged.
fn tag_to_pascal_case(tag: &str) -> String {
    let mut out = String::with_capacity(tag.len());
    let mut new_word = true;
    for ch in tag.chars() {
        if ch == '-' || ch == '_' {
            new_word = true;
            continue;
        }
        if new_word && ch.is_ascii_alphabetic() {
            out.extend(ch.to_ascii_uppercase().to_string().chars());
            new_word = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parse a template HTML string into r3_ast nodes with SELECTORLESS tokenization enabled, so
/// `<Foo>` / `@Foo` usages surface as real [`crate::template::r3_ast::Node::Component`] /
/// `Node::Directive` nodes (rather than plain elements). Used purely to drive auto-import
/// dependency resolution via the selectorless binder; the main template-instruction pipeline keeps
/// its own non-selectorless parse so its emitted instruction stream is unchanged.
pub fn parse_template_selectorless(template_html: &str) -> Vec<crate::template::r3_ast::Node> {
    let options = crate::ml_parser::TokenizeOptions {
        tokenize_expansion_forms: true,
        selectorless_enabled: true,
        ..crate::ml_parser::TokenizeOptions::default()
    };
    let parse_result =
        crate::ml_parser::HtmlParser::parse(template_html, "template.html", &options);
    let mut binding_parser = BindingParser::new();
    let r3 = html_ast_to_render3_ast(
        &parse_result.root_nodes,
        &mut binding_parser,
        Render3ParseOptions::default(),
    );
    r3.nodes
}

/// AUTO-IMPORT (import-less / selectorless authoring): resolve the component's template
/// dependencies *from template usage* instead of a manual `imports`/`declarations` array.
///
/// Given the set of identifiers the author imported (and any local component class names), this
/// registers each candidate name in a selectorless [`crate::binder::SelectorlessMatcher`], binds
/// the template through [`crate::binder::R3TargetBinder`], and returns one
/// [`R3TemplateDependencyMetadata`] (`kind: Directive`, `type: <Foo>`) per candidate that the
/// binder actually matched against a `<Foo>` / `@Foo` selectorless node in the template.
///
/// A `<foo>` / `<foo-bar>` element written with the kebab/camel spelling of an imported class is
/// also matched, by PascalCase-folding element tags before consulting the candidate set; such an
/// element does not parse as a selectorless `Component` node, so it is resolved here directly.
///
/// Imports that are never referenced in the template are NOT emitted — this is the whole point of
/// the model: `dependencies` reflects real template usage, mirroring how the REPL's
/// `treat-to-ivy.ts` only added *used* imports to `dependencies`, but here via the AST + binder
/// rather than regex.
pub fn resolve_template_dependencies(
    candidate_names: &[String],
    nodes: &[crate::template::r3_ast::Node],
) -> Vec<R3TemplateDependencyMetadata> {
    use crate::binder::{
        DirectiveMatcher, DirectiveMeta, R3TargetBinder, SelectorlessMatcher, Target,
    };

    if candidate_names.is_empty() {
        return Vec::new();
    }

    // The candidate set, deduplicated while preserving first-seen order (drives emission order).
    let mut ordered: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for name in candidate_names {
        if seen.insert(name.clone()) {
            ordered.push(name.clone());
        }
    }

    // Register every candidate class name in the selectorless matcher (the Angular 22 keystone:
    // a `<Foo>` node matches an imported symbol iff their class names are equal).
    let mut matcher = SelectorlessMatcher::<DirectiveMeta>::new();
    for name in &ordered {
        matcher.add(name.clone(), DirectiveMeta::new(name.clone(), None, true));
    }

    let binder = R3TargetBinder::new(Some(DirectiveMatcher::Selectorless(matcher)));
    let bound = binder.bind(Target {
        template: Some(nodes),
        host: None,
    });

    // Class names the binder matched against `<Foo>` / `@Foo` selectorless nodes.
    let mut used: std::collections::HashSet<String> = bound
        .get_used_directives()
        .into_iter()
        .map(|d| d.name)
        .collect();

    // `<foo>` / `<foo-bar>` / `<fooBar>` elements: PascalCase-fold each element tag and treat a hit
    // against the candidate set as a usage (these parse as `Element`, not selectorless `Component`,
    // nodes). The candidate import name is itself folded (so a camelCase import like `greetingCard`
    // — folding to `GreetingCard` — matches a `<greetingCard>`/`<greeting-card>`/`<GreetingCard>`
    // tag), and the ORIGINAL (unfolded) name is recorded so the final `dependencies` filter, which
    // keys on the import name, still emits it. Without folding both sides a camelCase-imported
    // selectorless component is silently dropped from `dependencies` and renders as an empty element.
    let candidate_by_pascal: std::collections::HashMap<String, &str> = ordered
        .iter()
        .map(|n| (tag_to_pascal_case(n), n.as_str()))
        .collect();
    collect_element_tag_usages(nodes, &candidate_by_pascal, &mut used);

    // Emit one dependency per matched candidate, in candidate (import) order.
    ordered
        .into_iter()
        .filter(|name| used.contains(name))
        .map(|name| R3TemplateDependencyMetadata {
            kind: crate::view::compiler::R3TemplateDependencyKind::Directive,
            ty: o::variable(name, None),
        })
        .collect()
}

/// Walk the template tree and record any `Element` whose tag — once PascalCase-folded — is in the
/// candidate class-name set. Mirrors the selectorless match for kebab/camel-spelled usages.
fn collect_element_tag_usages(
    nodes: &[crate::template::r3_ast::Node],
    candidates: &std::collections::HashMap<String, &str>,
    used: &mut std::collections::HashSet<String>,
) {
    use crate::template::r3_ast::Node;
    for node in nodes {
        match node {
            Node::Element(el) => {
                // Only a CUSTOM element tag — one that carries an uppercase letter (`greetingCard`)
                // or a hyphen (`greeting-card`) — can be a selectorless component reference. A plain
                // lowercase HTML element (`input`, `div`, `span`) is NEVER a component, even when a
                // same-named symbol is imported (e.g. the `input()` signal-forms function): folding
                // `<input>` to `Input` must not match an `input` import.
                let is_custom_tag =
                    el.name.contains('-') || el.name.chars().any(|c| c.is_ascii_uppercase());
                if is_custom_tag {
                    let pascal = tag_to_pascal_case(&el.name);
                    if let Some(orig) = candidates.get(&pascal) {
                        used.insert((*orig).to_string());
                    }
                }
                // ATTRIBUTE-selector auto-import: an imported directive whose class name folds to
                // an attribute used on this element (e.g. `RouterLink` ↔ `routerLink="/x"`, or the
                // bound form `[routerLink]`) is a real template usage and must land in
                // `dependencies`. Angular's ngtsc matches the directive's `[routerLink]` selector
                // against the attribute via the SelectorMatcher; lacking the external `.d.ts`
                // selector here, we fold attribute/input/output NAMES to PascalCase and treat a hit
                // against the imported candidate set as that same match (the Angular naming
                // convention `RouterLink → [routerLink]` holds for every attribute-selector directive).
                collect_attr_name_usages(
                    el.attributes.iter().map(|a| a.name.as_str()),
                    candidates,
                    used,
                );
                collect_attr_name_usages(
                    el.inputs.iter().map(|i| i.name.as_str()),
                    candidates,
                    used,
                );
                collect_attr_name_usages(
                    el.outputs.iter().map(|o| o.name.as_str()),
                    candidates,
                    used,
                );
                collect_element_tag_usages(&el.children, candidates, used);
            }
            Node::Component(c) => collect_element_tag_usages(&c.children, candidates, used),
            Node::Template(t) => {
                // Structural-directive / `ng-template` attribute usages match the same way.
                collect_attr_name_usages(
                    t.attributes.iter().map(|a| a.name.as_str()),
                    candidates,
                    used,
                );
                collect_attr_name_usages(
                    t.inputs.iter().map(|i| i.name.as_str()),
                    candidates,
                    used,
                );
                collect_attr_name_usages(
                    t.outputs.iter().map(|o| o.name.as_str()),
                    candidates,
                    used,
                );
                for ta in &t.template_attrs {
                    let name = match ta {
                        crate::template::r3_ast::TemplateAttr::Bound(b) => b.name.as_str(),
                        crate::template::r3_ast::TemplateAttr::Text(tx) => tx.name.as_str(),
                    };
                    collect_attr_name_usages(std::iter::once(name), candidates, used);
                }
                collect_element_tag_usages(&t.children, candidates, used);
            }
            Node::Content(c) => collect_element_tag_usages(&c.children, candidates, used),
            Node::DeferredBlock(b) => collect_element_tag_usages(&b.children, candidates, used),
            Node::DeferredBlockPlaceholder(b) => {
                collect_element_tag_usages(&b.children, candidates, used)
            }
            Node::DeferredBlockLoading(b) => {
                collect_element_tag_usages(&b.children, candidates, used)
            }
            Node::DeferredBlockError(b) => {
                collect_element_tag_usages(&b.children, candidates, used)
            }
            Node::SwitchBlock(b) => {
                for g in &b.groups {
                    collect_element_tag_usages(&g.children, candidates, used);
                }
            }
            Node::ForLoopBlock(b) => {
                collect_element_tag_usages(&b.children, candidates, used);
                if let Some(empty) = &b.empty {
                    collect_element_tag_usages(&empty.children, candidates, used);
                }
            }
            Node::IfBlock(b) => {
                for branch in &b.branches {
                    collect_element_tag_usages(&branch.children, candidates, used);
                }
            }
            _ => {}
        }
    }
}

/// Fold each attribute name to PascalCase and record a usage for any imported candidate class name
/// it equals. This is the attribute-selector arm of auto-import: a directive imported by class name
/// (`RouterLink`) is "used" when its conventional attribute selector (`[routerLink]`) appears on a
/// template node, mirroring how Angular's SelectorMatcher matches `[routerLink]` against the
/// element's `routerLink` attribute. Bound (`[x]`) and plain (`x="…"`) attributes share the same
/// underlying name, so both forms resolve identically here.
fn collect_attr_name_usages<'a>(
    names: impl Iterator<Item = &'a str>,
    candidates: &std::collections::HashMap<String, &str>,
    used: &mut std::collections::HashSet<String>,
) {
    for name in names {
        // Skip Angular structural/template microsyntax + binding sugar prefixes that are never
        // part of a directive's attribute name.
        let bare = name.trim_start_matches('*');
        if bare.is_empty() {
            continue;
        }
        let pascal = tag_to_pascal_case(bare);
        // Attribute-selector auto-import matches only a PascalCase directive candidate
        // (`RouterLink` ↔ `routerLink`). A non-Pascal import (a function like `input`) must NOT be
        // pulled in by a same-folding attribute name, so require the candidate to equal its own fold.
        if let Some(orig) = candidates.get(&pascal) {
            if *orig == pascal {
                used.insert((*orig).to_string());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// `@defer` dependency analysis.
//
// Angular splits a component's template directive/pipe dependencies into the EAGER set (everything
// outside a `@defer` block's MAIN body — including its `@placeholder`/`@loading`/`@error` views,
// which render before/while the deferred content loads) and the per-block LAZY set (the directives
// used only in a `@defer` block's main body). The runtime `dependencies` array carries the eager
// set; each block's lazy set becomes a `() => [Dep, …]` resolver thunk passed as the third
// `ɵɵdefer(slot, mainSlot, resolverFn, …)` argument (`render3/view/compiler.ts`
// `compileDeferResolverFunction` + compiler-cli `resolveDeferBlocks`/`compileDeferBlocks`).
//
// We reproduce the eager/lazy split by re-running the SAME matching passes the eager
// `dependencies` array is built from (the selectorless class-name + element-tag matcher and the
// CSS-selector binder) over two derived node sets:
//   * the EAGER PROJECTION — the template with every `@defer` MAIN body emptied — to learn which
//     deps are referenced eagerly, and
//   * each `@defer` block's MAIN body in isolation — to learn that block's lazy deps.
// The binder (`R3TargetBinder`) is already `@defer`-aware (it flips `is_in_defer_block` only inside
// the main body, treating the secondary views as eager), so the eager projection mirrors
// `getEagerlyUsedDirectives` exactly.
// ---------------------------------------------------------------------------

/// One `@defer` block's lazy dependency set, keyed by the block's main-body source span (so the
/// template builder can match it back to the block it is lowering, independent of document order /
/// nesting). `names` is ordered by the component's import/declaration order, matching how Angular's
/// `compileDeferResolverFunction` walks `meta.dependencies`.
#[derive(Debug, Clone, Default)]
pub struct DeferBlockDeps {
    /// `(start, end)` of the block's `main_block_span` (the `DeferredBlock` AST node's span).
    pub span: (u32, u32),
    pub names: Vec<String>,
}

/// The eager/lazy dependency split for a template (see module note above).
#[derive(Debug, Clone, Default)]
pub struct DeferDependencyInfo {
    /// Names referenced EAGERLY (outside any `@defer` main body). The runtime `dependencies` array
    /// keeps exactly these.
    pub eager_names: std::collections::HashSet<String>,
    /// Per-`@defer`-block lazy dependency sets, in document order.
    pub blocks: Vec<DeferBlockDeps>,
}

impl DeferDependencyInfo {
    /// Whether the template carries any `@defer` block at all (when not, the caller leaves the
    /// existing PerComponent / no-resolver path untouched — byte-identical to before this analysis).
    pub fn has_defer_blocks(&self) -> bool {
        !self.blocks.is_empty()
    }
}

/// Recursively clone `nodes`, emptying every `@defer` block's MAIN body (`children`) while keeping
/// its `@placeholder`/`@loading`/`@error` sub-blocks (which render eagerly). Recurses through every
/// container so nested `@defer` blocks (inside `@if`/`@for`/`@switch`/elements/etc.) are handled.
fn strip_defer_main_bodies(nodes: &[crate::template::r3_ast::Node]) -> Vec<crate::template::r3_ast::Node> {
    use crate::template::r3_ast::Node;
    nodes
        .iter()
        .map(|node| {
            let mut n = node.clone();
            match &mut n {
                Node::Element(el) => el.children = strip_defer_main_bodies(&el.children),
                Node::Component(c) => c.children = strip_defer_main_bodies(&c.children),
                Node::Template(t) => t.children = strip_defer_main_bodies(&t.children),
                Node::Content(c) => c.children = strip_defer_main_bodies(&c.children),
                Node::SwitchBlock(b) => {
                    for g in &mut b.groups {
                        g.children = strip_defer_main_bodies(&g.children);
                    }
                }
                Node::ForLoopBlock(b) => {
                    b.children = strip_defer_main_bodies(&b.children);
                    if let Some(empty) = &mut b.empty {
                        empty.children = strip_defer_main_bodies(&empty.children);
                    }
                }
                Node::IfBlock(b) => {
                    for branch in &mut b.branches {
                        branch.children = strip_defer_main_bodies(&branch.children);
                    }
                }
                Node::DeferredBlock(d) => {
                    // The MAIN body is the lazy region — empty it. Its secondary views are eager,
                    // so keep them (but still recurse, in case THEY contain nested `@defer` blocks).
                    d.children = Vec::new();
                    if let Some(p) = &mut d.placeholder {
                        p.children = strip_defer_main_bodies(&p.children);
                    }
                    if let Some(l) = &mut d.loading {
                        l.children = strip_defer_main_bodies(&l.children);
                    }
                    if let Some(e) = &mut d.error {
                        e.children = strip_defer_main_bodies(&e.children);
                    }
                }
                _ => {}
            }
            n
        })
        .collect()
}

/// Collect every `@defer` block's `(main_block_span, main-body children)` in document (pre-order)
/// order, recursing through all containers (including secondary views and nested `@defer` blocks).
fn collect_defer_blocks<'a>(
    nodes: &'a [crate::template::r3_ast::Node],
    out: &mut Vec<(&'a crate::template::r3_ast::DeferredBlock, (u32, u32))>,
) {
    use crate::template::r3_ast::Node;
    for node in nodes {
        match node {
            Node::Element(el) => collect_defer_blocks(&el.children, out),
            Node::Component(c) => collect_defer_blocks(&c.children, out),
            Node::Template(t) => collect_defer_blocks(&t.children, out),
            Node::Content(c) => collect_defer_blocks(&c.children, out),
            Node::SwitchBlock(b) => {
                for g in &b.groups {
                    collect_defer_blocks(&g.children, out);
                }
            }
            Node::ForLoopBlock(b) => {
                collect_defer_blocks(&b.children, out);
                if let Some(empty) = &b.empty {
                    collect_defer_blocks(&empty.children, out);
                }
            }
            Node::IfBlock(b) => {
                for branch in &b.branches {
                    collect_defer_blocks(&branch.children, out);
                }
            }
            Node::DeferredBlock(d) => {
                let span = (d.main_block_span.start, d.main_block_span.end);
                out.push((d, span));
                // Recurse into the main body + secondary views for nested `@defer` blocks.
                collect_defer_blocks(&d.children, out);
                if let Some(p) = &d.placeholder {
                    collect_defer_blocks(&p.children, out);
                }
                if let Some(l) = &d.loading {
                    collect_defer_blocks(&l.children, out);
                }
                if let Some(e) = &d.error {
                    collect_defer_blocks(&e.children, out);
                }
            }
            _ => {}
        }
    }
}

/// Match the directive/pipe candidates against a SUBTREE of template nodes, returning the matched
/// class names in candidate (import/declaration) order. This is the shared kernel behind both the
/// eager-projection and per-`@defer`-block analyses: it runs the SAME two matching passes the main
/// `dependencies` array is built from — the selectorless class-name + element-tag matcher
/// ([`resolve_template_dependencies`]) and the CSS-selector binder
/// ([`crate::binder::resolve_selector_dependencies`]) — and merges their hits, preserving the
/// canonical emission order (import order for the selectorless hits, then selector-only matches).
fn match_dependencies_in(
    nodes: &[crate::template::r3_ast::Node],
    imported_names: &[String],
    selector_candidates: &[crate::binder::SelectorDirective],
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for dep in resolve_template_dependencies(imported_names, nodes) {
        if let ExprKind::ReadVar { name } = &dep.ty.kind {
            if seen.insert(name.clone()) {
                out.push(name.clone());
            }
        }
    }
    if !selector_candidates.is_empty() {
        for name in crate::binder::resolve_selector_dependencies(nodes, selector_candidates) {
            if seen.insert(name.clone()) {
                out.push(name);
            }
        }
    }
    out
}

/// Compute the eager/lazy dependency split for a template (see module note above). `imported_names`
/// are the selectorless class-name + element-tag candidates; `selector_candidates` the CSS-selector
/// directives — exactly the two candidate sets the main `dependencies` array is matched from.
pub fn compute_defer_dependencies(
    nodes: &[crate::template::r3_ast::Node],
    imported_names: &[String],
    selector_candidates: &[crate::binder::SelectorDirective],
) -> DeferDependencyInfo {
    let mut info = DeferDependencyInfo::default();

    // EAGER: match over the template with every `@defer` main body emptied. The binder treats the
    // secondary (`@placeholder`/`@loading`/`@error`) views as eager, and the element-tag walk only
    // sees the kept nodes, so this yields exactly `getEagerlyUsedDirectives` ∪ eager element/attr
    // usages.
    let eager_nodes = strip_defer_main_bodies(nodes);
    info.eager_names = match_dependencies_in(&eager_nodes, imported_names, selector_candidates)
        .into_iter()
        .collect();

    // LAZY: for each `@defer` block, match over its MAIN body in isolation. The block's lazy
    // resolver lists exactly these (`resolveDeferBlocks` binds `deferBlock.children` separately).
    let mut blocks = Vec::new();
    collect_defer_blocks(nodes, &mut blocks);
    for (block, span) in blocks {
        let names = match_dependencies_in(&block.children, imported_names, selector_candidates);
        info.blocks.push(DeferBlockDeps { span, names });
    }

    info
}

// ---------------------------------------------------------------------------
// Wrapped-node resolution pass.
//
// Walks the compiled definition expression and replaces every `WrappedNode(handle)` leaf with the
// host expression registered for that handle (the class identifier), so the JS emitter — which has
// no side table — prints the wrapped node as the class identifier. This is the in-pipeline analogue
// of ngtsc handing the `WrappedNodeExpr`'s host TS node straight to the TypeScript printer.
// ---------------------------------------------------------------------------

/// Resolve every `WrappedNode` leaf in `expr` to its registered host expression, in place.
fn resolve_wrapped_nodes(expr: &mut Expr) {
    if let ExprKind::WrappedNode(handle) = expr.kind {
        if let Some(mut resolved) = resolved_wrapped_node(handle) {
            // A registered node may itself contain wrapped nodes (none today, but resolve to be safe).
            resolve_wrapped_nodes(&mut resolved);
            let comments = std::mem::take(&mut expr.meta.leading_comments);
            *expr = resolved;
            // Preserve any leading comments that sat on the WrappedNode reference.
            expr.meta.leading_comments.splice(0..0, comments);
        }
        return;
    }
    match &mut expr.kind {
        ExprKind::ReadVar { .. }
        | ExprKind::Literal(_)
        | ExprKind::External { .. }
        | ExprKind::RegExpLiteral { .. }
        | ExprKind::TemplateLiteralElement(_)
        | ExprKind::WrappedNode(_) => {}
        ExprKind::Typeof(e)
        | ExprKind::Void(e)
        | ExprKind::Not(e)
        | ExprKind::Parenthesized(e)
        | ExprKind::Spread(e)
        | ExprKind::Unary { expr: e, .. } => resolve_wrapped_nodes(e),
        ExprKind::Invoke { callee, args, .. } => {
            resolve_wrapped_nodes(callee);
            args.iter_mut().for_each(resolve_wrapped_nodes);
        }
        ExprKind::TaggedTemplate { tag, template } => {
            resolve_wrapped_nodes(tag);
            resolve_wrapped_nodes(template);
        }
        ExprKind::New { class_expr, args } => {
            resolve_wrapped_nodes(class_expr);
            args.iter_mut().for_each(resolve_wrapped_nodes);
        }
        ExprKind::TemplateLiteral { expressions, .. } => {
            expressions.iter_mut().for_each(resolve_wrapped_nodes);
        }
        ExprKind::LocalizedString { expressions, .. } => {
            expressions.iter_mut().for_each(resolve_wrapped_nodes);
        }
        ExprKind::Conditional {
            condition,
            true_case,
            false_case,
        } => {
            resolve_wrapped_nodes(condition);
            resolve_wrapped_nodes(true_case);
            if let Some(f) = false_case {
                resolve_wrapped_nodes(f);
            }
        }
        ExprKind::DynamicImport { url, .. } => {
            if let ImportUrl::Expr(e) = url {
                resolve_wrapped_nodes(e);
            }
        }
        ExprKind::Function { statements, .. } => {
            statements.iter_mut().for_each(resolve_wrapped_nodes_stmt);
        }
        ExprKind::Arrow { body, .. } => match body {
            ArrowBody::Expr(e) => resolve_wrapped_nodes(e),
            ArrowBody::Block(stmts) => stmts.iter_mut().for_each(resolve_wrapped_nodes_stmt),
        },
        ExprKind::Binary { lhs, rhs, .. } => {
            resolve_wrapped_nodes(lhs);
            resolve_wrapped_nodes(rhs);
        }
        ExprKind::ReadProp { receiver, .. } => resolve_wrapped_nodes(receiver),
        ExprKind::ReadKey { receiver, index, .. } => {
            resolve_wrapped_nodes(receiver);
            resolve_wrapped_nodes(index);
        }
        ExprKind::LiteralArray(entries) => entries.iter_mut().for_each(resolve_wrapped_nodes),
        ExprKind::LiteralMap { entries, .. } => {
            for entry in entries {
                match entry {
                    LiteralMapEntry::Property { value, .. } => resolve_wrapped_nodes(value),
                    LiteralMapEntry::Spread { expression } => resolve_wrapped_nodes(expression),
                }
            }
        }
        ExprKind::Comma(parts) => parts.iter_mut().for_each(resolve_wrapped_nodes),
    }
}

/// Resolve every `WrappedNode` leaf inside a statement.
fn resolve_wrapped_nodes_stmt(stmt: &mut Stmt) {
    match &mut stmt.kind {
        StmtKind::DeclareVar { value, .. } => {
            if let Some(v) = value {
                resolve_wrapped_nodes(v);
            }
        }
        StmtKind::DeclareFunction { statements, .. } => {
            statements.iter_mut().for_each(resolve_wrapped_nodes_stmt);
        }
        StmtKind::Expression(e) | StmtKind::Return(e) => resolve_wrapped_nodes(e),
        StmtKind::If {
            condition,
            true_case,
            false_case,
        } => {
            resolve_wrapped_nodes(condition);
            true_case.iter_mut().for_each(resolve_wrapped_nodes_stmt);
            false_case.iter_mut().for_each(resolve_wrapped_nodes_stmt);
        }
    }
}

/// Build a minimal-but-faithful [`R3DirectiveMetadata`] base for a standalone component.
fn base_metadata(selector: &str, class_name: &str) -> R3DirectiveMetadata {
    R3DirectiveMetadata {
        name: class_name.to_string(),
        ty: class_ref(class_name),
        type_argument_count: 0,
        // The type source span only drives diagnostics / `.d.ts` location info, neither of which
        // this emit-only pipeline produces; an empty span is faithful for the emitted definition.
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

    // Stable wrapped-node handles per compilation: clear the side table so this component's class
    // reference always lands at handle 0 (and the table never grows across repeated calls).
    reset_wrapped_nodes();

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
        // Defer metadata is emitted per-component. With no `@defer` blocks in the template there are
        // no deferrable dependencies, so the resolver function is `None` — which is exactly what
        // Angular emits for a defer-free `PerComponent` template. (A template that *did* contain
        // `@defer` blocks would need the imports-driven defer dependency resolver to build a real
        // `dependencies_fn`; this selectorless pipeline carries no import scope — see `remaining`.)
        defer: R3ComponentDeferMetadata::PerComponent {
            dependencies_fn: None,
        },
        declaration_list_emit_mode: DeclarationListEmitMode::Direct,
        styles: Vec::new(),
        external_styles: None,
        // No styles: `compile_component_from_metadata` normalizes Emulated -> None encapsulation
        // (the encapsulation field is only emitted when styles are present).
        encapsulation: ViewEncapsulation::Emulated,
        animations: None,
        view_providers: None,
        relative_context_file_path: String::new(),
        i18n_use_external_ids: false,
        // This authoring model has no `@Component` decorator to read a `changeDetection` from, so the
        // implicit Ivy default (OnPush) is used; it matches the runtime default and so is not emitted
        // into the definition, keeping the output minimal.
        change_detection: Some(ChangeDetection::Strategy(ChangeDetectionStrategy::OnPush)),
        relative_template_path: None,
        has_directive_dependencies: false,
        raw_imports: None,
        foreign_imports: None,
        // This selectorless pipeline carries no import scope (see above), so no imported pipe can
        // be resolved here; the source-driven `compile_component_meta` path threads the real
        // `imports` for pipe-dependency resolution.
        imported_directive_names: Vec::new(),
    };

    // 4. Assemble the definition (real template builder + stub host bindings).
    let mut template_builder = RealTemplateBuilder;
    let mut host_builder = StubHostBindingsBuilder;
    let mut pool_statements = Vec::new();
    let mut compiled: R3CompiledExpression = compile_component_from_metadata(
        &mut meta,
        &mut template_builder,
        &mut host_builder,
        &mut pool_statements,
    );

    // 5. Resolve the `WrappedNode` class references back to their host identifier expressions, then
    //    emit the definition expression to JS (the emitter has no view of the wrapped-node side
    //    table, so this substitution must happen first — ngtsc hands the wrapped TS node straight to
    //    the printer instead).
    resolve_wrapped_nodes(&mut compiled.expression);

    // Angular emits the hoisted nested-view functions (and any other `ConstantPool.statements`)
    // as top-level sibling declarations BEFORE the `ɵɵdefineComponent({…})` call. Mirror that:
    // print the pool statements first, then the definition expression. The wrapped-node side table
    // must be resolved inside the pool statements too (they reference the component class etc.).
    let code = if pool_statements.is_empty() {
        emit_expression(&compiled.expression)
    } else {
        for stmt in &mut pool_statements {
            resolve_wrapped_nodes_stmt(stmt);
        }
        let mut out = emit_statements(&pool_statements);
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&emit_expression(&compiled.expression));
        out
    };

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
    /// fidelity for bindings / control-flow is exercised by the per-module `view` tests and
    /// the parity oracle — here we only assert "does not panic" + a valid definition is produced.)
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
