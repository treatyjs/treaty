//! The decorator-compiler **registry** — the "join" of the `decorators` layer.
//!
//! Mirrors `apps/rust/authoring`'s `AuthoringPlugin` / `AuthoringRegistry`: every Angular
//! decorator kind Treaty emits a definition for is just a [`DecoratorCompiler`] plugin. A plugin
//! declares the [`AngularDecoratorKind`] it owns and knows how to compile one decorated class —
//! supplied as a [`ClassMeta`] together with the cross-class [`CompileCtx`] — into a
//! [`CompiledDef`] (the decomposed Ivy `ɵɵdefine*` emit).
//!
//! The per-FILE driver in `source_compile` (in the facade crate `treaty_ivy`) scans every decorated class and dispatches
//! each one through [`DecoratorRegistry::for_kind`] → [`DecoratorCompiler::compile`] instead of a
//! hand-written `match`, so **adding a decorator kind is a registration, not an edit**. The
//! concrete plugins live next to the oxc metadata extraction they delegate to (in
//! `source_compile`), exactly as the authoring plugins live next to their format delegations.
//!
//! This is a structural refactor: the emitted definition is byte-identical to the prior
//! `match kind { … }` dispatch.

use oxc_ast::ast::{Class, Decorator, ObjectExpression};

use crate::factory::R3FactoryMetadata;
use crate::output_ast::{self as o, Expr, ParseSourceSpan};

/// The Angular decorator kinds the front-end recognizes on a top-level class. The FIRST recognized
/// decorator on a class wins (ngtsc's single-trait-per-class rule); see
/// `source_compile` (in the facade crate `treaty_ivy`)'s class scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AngularDecoratorKind {
    Component,
    Directive,
    Pipe,
    NgModule,
    Injectable,
}

/// One decorated class, as handed to a [`DecoratorCompiler`]. Borrows the oxc nodes (the `class`
/// declaration and its leading `decorator`), the decorator's options object (`@Foo({...})` →
/// `Some(obj)`; bare `@Foo` → `None`), and carries the class name + the original-source span of the
/// class identifier (the anchor the additive source map maps the emitted `type: <ClassName>` back
/// to).
pub struct ClassMeta<'a> {
    pub class: &'a Class<'a>,
    pub decorator: &'a Decorator<'a>,
    pub object: Option<&'a ObjectExpression<'a>>,
    pub class_name: String,
    pub class_name_span: ParseSourceSpan,
}

/// Host-resolved external content for ONE `@Component` class: the template string its `templateUrl`
/// resolves to and the style strings its `styleUrls`/`styleUrl` resolve to. File I/O and path
/// resolution are HOST/bundler concerns; the compiler consumes the already-read contents. See
/// `source_compile`'s `ResolvedComponentContent` (the facade re-export of this type).
#[derive(Debug, Clone, Default)]
pub struct ResolvedComponentContent {
    /// The resolved template HTML for a `templateUrl` component (`None` leaves it erroring).
    pub template: Option<String>,
    /// The resolved style strings for `styleUrls`/`styleUrl`, in declaration order.
    pub styles: Vec<String>,
}

/// Per-file map from a `@Component` class name to its host-resolved external content. Keyed by class
/// name so multi-class files resolve each component independently.
pub type ResolvedContentMap = std::collections::HashMap<String, ResolvedComponentContent>;

/// Cross-class context shared by every class in a file, needed to resolve template dependencies in
/// a multi-class module. `auto_import_candidates` is the union of the file's imported names and the
/// sibling class names (for selectorless auto-import); `sibling_directives` is every sibling
/// `@Directive`/`@Component` carrying a non-empty `selector` (for cross-class CSS-selector matching).
/// `resolved_content` carries, per component class name, the host-resolved `templateUrl`/`styleUrls`
/// contents (`None` when the caller supplied no resolution channel — inline-only compilation).
/// `default_selector` is the fallback element selector a SELECTORLESS `@Component` adopts when its
/// decorator declares no `selector` (the Treaty convention: a filename-derived kebab tag), so a
/// bootstrapped selectorless `.ts` component renders a real host tag instead of Angular's
/// `ng-component` no-selector default. It is `None` for the golden-corpus / inline compile paths,
/// which leaves a selectorless component's `selector` exactly `None` (byte-identical emit), and it
/// is NEVER applied to a `@Directive` (directives are legitimately selectorless / class-only).
pub struct CompileCtx<'a> {
    pub auto_import_candidates: &'a [String],
    pub sibling_directives: &'a [crate::binder::SelectorDirective],
    pub resolved_content: Option<&'a ResolvedContentMap>,
    pub default_selector: Option<&'a str>,
    /// The `legacyOptionalChaining` Angular compiler option: when set, a safe-navigation host-binding
    /// value (`getData()?.id`) lowers to the classic guarded-temporary ternary rather than the native
    /// `?.` operator. `false` for every default compile path; only the option-carrying entry point
    /// (and the compliance corpus dump, which reads it per case) sets it.
    pub legacy_optional_chaining: bool,
}

/// The Ivy emit of ONE decorated class, decomposed so the original module can be re-assembled
/// around it (rather than replaced by it).
///
/// `def_expression` is the `ɵɵdefine*({...})` call render3 produces — byte-identical to the
/// historical bare-expression emit. `extra_statements` are the hoisted constant-pool consts /
/// nested template functions (and, for `@NgModule`, the `ɵɵsetNgModuleScope` /
/// `ɵɵregisterNgModuleType` side-effect statements). `factory` is the `ɵfac` metadata when the kind
/// carries one. The caller stitches these AROUND the kept (decorator-stripped) class declaration as
/// `<pool…>; X.ɵfac = <factory>; X.<static_member> = <def_expression>;`.
pub struct CompiledDef {
    pub class_name: String,
    /// The Ivy static property name the definition is assigned to (`ɵcmp`/`ɵdir`/`ɵpipe`/`ɵmod`).
    pub static_member: &'static str,
    pub def_expression: Expr,
    pub extra_statements: Vec<o::Stmt>,
    /// Whether `extra_statements` must be emitted AFTER the `X.<member> =` assignment. Component /
    /// directive / pipe hoist constant-pool consts the definition REFERENCES, so they come BEFORE
    /// (`false`); `@NgModule` emits `ɵɵsetNgModuleScope` / `ɵɵregisterNgModuleType` SIDE EFFECTS
    /// that run after the definition exists, so they come AFTER (`true`).
    pub extra_after_def: bool,
    /// `ɵfac` factory metadata, when the kind declares a factory (Component/Directive/Pipe/NgModule).
    pub factory: Option<R3FactoryMetadata>,
    /// Non-fatal diagnostics (e.g. template parse warnings) gathered while compiling this class.
    pub errors: Vec<String>,
}

/// A compiler front-end for ONE Angular decorator kind.
///
/// Implementations turn a decorated class ([`ClassMeta`]) + the cross-class [`CompileCtx`] into its
/// decomposed Ivy definition ([`CompiledDef`]). The [`DecoratorRegistry`] dispatches to a plugin by
/// the [`AngularDecoratorKind`] it claims via [`DecoratorCompiler::kind`]. Mirrors
/// `apps/rust/authoring::AuthoringPlugin`.
pub trait DecoratorCompiler {
    /// The decorator kind this plugin compiles.
    fn kind(&self) -> AngularDecoratorKind;

    /// Compile one decorated class into its decomposed Ivy definition, or a fatal diagnostic
    /// (`Err`) when the class carries metadata the front-end cannot model yet.
    fn compile(&self, class: &ClassMeta, ctx: &CompileCtx) -> Result<CompiledDef, String>;
}

/// A registry of [`DecoratorCompiler`] plugins, one per [`AngularDecoratorKind`].
///
/// The per-file driver resolves the plugin for each decorated class's kind via [`Self::for_kind`]
/// and calls [`DecoratorCompiler::compile`]. Mirrors `apps/rust/authoring::AuthoringRegistry`.
pub struct DecoratorRegistry {
    plugins: Vec<Box<dyn DecoratorCompiler>>,
}

impl DecoratorRegistry {
    /// An empty registry (no plugins).
    pub fn new() -> Self {
        Self { plugins: Vec::new() }
    }

    /// Register a decorator-compiler plugin. The LAST plugin registered for a given kind wins on
    /// [`Self::for_kind`] lookup (registration order otherwise does not matter, since each kind is
    /// dispatched independently).
    pub fn register(&mut self, plugin: Box<dyn DecoratorCompiler>) {
        self.plugins.push(plugin);
    }

    /// Look up the plugin that compiles `kind`, if any is registered.
    pub fn for_kind(&self, kind: AngularDecoratorKind) -> Option<&dyn DecoratorCompiler> {
        self.plugins
            .iter()
            .rev()
            .find(|p| p.kind() == kind)
            .map(|p| p.as_ref())
    }
}

impl Default for DecoratorRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trivial plugin that records its kind, so the registry's `for_kind` dispatch can be tested
    /// without standing up the full oxc extraction pipeline.
    struct StubCompiler(AngularDecoratorKind);
    impl DecoratorCompiler for StubCompiler {
        fn kind(&self) -> AngularDecoratorKind {
            self.0
        }
        fn compile(&self, _c: &ClassMeta, _ctx: &CompileCtx) -> Result<CompiledDef, String> {
            Err("stub".to_string())
        }
    }

    #[test]
    fn for_kind_resolves_each_registered_kind() {
        let mut registry = DecoratorRegistry::new();
        registry.register(Box::new(StubCompiler(AngularDecoratorKind::Component)));
        registry.register(Box::new(StubCompiler(AngularDecoratorKind::Pipe)));

        assert_eq!(
            registry
                .for_kind(AngularDecoratorKind::Component)
                .map(|p| p.kind()),
            Some(AngularDecoratorKind::Component)
        );
        assert_eq!(
            registry.for_kind(AngularDecoratorKind::Pipe).map(|p| p.kind()),
            Some(AngularDecoratorKind::Pipe)
        );
    }

    #[test]
    fn for_kind_unregistered_is_none() {
        let registry = DecoratorRegistry::new();
        assert!(registry.for_kind(AngularDecoratorKind::NgModule).is_none());
    }

    #[test]
    fn for_kind_resolves_when_multiple_share_a_kind() {
        // Two plugins claim the same kind (a host overriding a built-in compiler — the extensibility
        // the registry exists to support). `for_kind` still resolves the kind (to the LAST
        // registered, per `register`'s contract) rather than returning `None` or panicking.
        let mut registry = DecoratorRegistry::new();
        registry.register(Box::new(StubCompiler(AngularDecoratorKind::Directive)));
        registry.register(Box::new(StubCompiler(AngularDecoratorKind::Directive)));
        assert_eq!(
            registry
                .for_kind(AngularDecoratorKind::Directive)
                .map(|p| p.kind()),
            Some(AngularDecoratorKind::Directive)
        );
    }
}
