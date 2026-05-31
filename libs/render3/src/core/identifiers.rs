//! The full `ɵɵ*` Ivy runtime identifier table (ExternalReference constants).
//!
//! PORT TARGET: see `migration/render3-specs/14-identifiers.md`
//! Source: `tools/angular-ref/packages/compiler/src/render3/r3_identifiers.ts`
//!
//! This is Angular's `Identifiers` class (aliased `R3` by consumers): the single
//! source of truth for every runtime symbol the render3 (Ivy) compiler can
//! reference in generated code. Each entry is an [`ExternalReference`] — a
//! `{name, module_name}` pair naming a function/class/enum exported (usually
//! under the `ɵɵ` / `ɵ` private prefix) from `@angular/core`.
//!
//! The module emits no code itself; it is a constant lookup table. It depends
//! only on [`crate::output_ast::ExternalReference`].
//!
//! Representation: a type-safe [`R3`] enum with one variant per static field,
//! plus [`R3::reference`] returning the owned [`ExternalReference`]. Because the
//! port pins `ExternalReference.name` to a required `String` (not `string |
//! null`), the values cannot be `const`; they are produced on demand via owned
//! `String`s. A `'static` `&str` table backs each variant so construction is a
//! single allocation of the name plus the module name.
//!
//! Pinned to Angular `22.1.0-next.0`. Field name vs wire name diverge in several
//! cases (`templateCreate` -> `ɵɵtemplate`, the `animation*` family -> `ɵɵanimate*`,
//! `ɵɵdefineInjectable` whose field is itself prefixed); the wire string is copied
//! verbatim from the TS source and never derived from the field name.
//!
//! Note the three prefix families, preserved byte-for-byte (the `ɵ` is U+0275,
//! Latin small letter barred O):
//!   * `ɵɵ` (double) — most runtime instructions.
//!   * `ɵ`  (single) — `setClassMetadata*`, `setClassDebugInfo`, the type-checking tail.
//!   * none — public-API names: `forwardRef`, `ChangeDetectionStrategy`, the decorators, etc.

use crate::output_ast::ExternalReference;

/// `@angular/core` — the module every identifier in this table resolves against.
const CORE: &str = "@angular/core";

/// Build a `{name, module_name: Some(CORE)}` reference. Owned, since
/// `ExternalReference.name` is a `String` (the port pins it non-null).
fn core_ref(name: &str) -> ExternalReference {
    ExternalReference {
        name: name.to_string(),
        module_name: Some(CORE.to_string()),
    }
}

/// Type-safe enumeration of every runtime symbol in Angular's `Identifiers`
/// table. Callers reference `R3::Element` rather than a stringly-typed key, and
/// obtain the [`ExternalReference`] via [`R3::reference`].
///
/// Variant names mirror the TS static-field names in PascalCase. Where the TS
/// field name already starts with the `ɵɵ` prefix (`ɵɵdefineInjectable`) the
/// variant is named after its purpose (`DefineInjectableField`) to stay a valid
/// Rust identifier; see the doc on that variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum R3 {
    // Namespace
    /// `core` -> namespace import (`import * as i0 from '@angular/core'`); name is empty.
    Core,
    NamespaceHTML,
    NamespaceMathML,
    NamespaceSVG,

    // Element / DOM creation
    Element,
    ElementStart,
    ElementEnd,
    ForeignComponent,
    DomElement,
    DomElementStart,
    DomElementEnd,
    DomElementContainer,
    DomElementContainerStart,
    DomElementContainerEnd,
    DomTemplate,
    DomListener,
    ElementContainerStart,
    ElementContainerEnd,
    ElementContainer,

    // Advance / view
    Advance,
    NextContext,
    ResetView,
    GetCurrentView,
    RestoreView,
    Reference,
    EnableBindings,
    DisableBindings,
    /// `templateCreate` field -> wire name `ɵɵtemplate`.
    TemplateCreate,

    // Property / attribute / style binding
    SyntheticHostProperty,
    SyntheticHostListener,
    Attribute,
    ClassProp,
    StyleMap,
    ClassMap,
    StyleProp,
    DomProperty,
    AriaProperty,
    Property,
    Control,
    ControlCreate,

    // Interpolation (expression form)
    Interpolate,
    Interpolate1,
    Interpolate2,
    Interpolate3,
    Interpolate4,
    Interpolate5,
    Interpolate6,
    Interpolate7,
    Interpolate8,
    InterpolateV,

    // Text + text interpolation
    Text,
    TextInterpolate,
    TextInterpolate1,
    TextInterpolate2,
    TextInterpolate3,
    TextInterpolate4,
    TextInterpolate5,
    TextInterpolate6,
    TextInterpolate7,
    TextInterpolate8,
    TextInterpolateV,

    // Defer (@defer)
    Defer,
    DeferWhen,
    DeferOnIdle,
    DeferOnImmediate,
    DeferOnTimer,
    DeferOnHover,
    DeferOnInteraction,
    DeferOnViewport,
    DeferPrefetchWhen,
    DeferPrefetchOnIdle,
    DeferPrefetchOnImmediate,
    DeferPrefetchOnTimer,
    DeferPrefetchOnHover,
    DeferPrefetchOnInteraction,
    DeferPrefetchOnViewport,
    DeferHydrateWhen,
    DeferHydrateNever,
    DeferHydrateOnIdle,
    DeferHydrateOnImmediate,
    DeferHydrateOnTimer,
    DeferHydrateOnHover,
    DeferHydrateOnInteraction,
    DeferHydrateOnViewport,
    DeferEnableTimerScheduling,
    EnableIncrementalHydrationRuntime,

    // Control flow (@if / @switch / @for)
    ConditionalCreate,
    ConditionalBranchCreate,
    Conditional,
    Repeater,
    RepeaterCreate,
    RepeaterTrackByIndex,
    RepeaterTrackByIdentity,
    ComponentInstance,

    // Pure functions / pipes
    PureFunction0,
    PureFunction1,
    PureFunction2,
    PureFunction3,
    PureFunction4,
    PureFunction5,
    PureFunction6,
    PureFunction7,
    PureFunction8,
    PureFunctionV,
    PipeBind1,
    PipeBind2,
    PipeBind3,
    PipeBind4,
    PipeBindV,
    Pipe,

    // Animations (field name != wire name: animation* -> ɵɵanimate*)
    AnimationEnterListener,
    AnimationLeaveListener,
    AnimationEnter,
    AnimationLeave,

    // i18n
    I18n,
    I18nAttributes,
    I18nExp,
    I18nStart,
    I18nEnd,
    I18nApply,
    I18nPostprocess,

    // Projection / content
    Projection,
    ProjectionDef,

    // Dependency injection
    Inject,
    InjectAttribute,
    DirectiveInject,
    InvalidFactory,
    InvalidFactoryDep,
    TemplateRefExtractor,
    ForwardRef,
    ResolveForwardRef,
    GetInheritedFactory,
    ResolveWindow,
    ResolveDocument,
    ResolveBody,
    GetComponentDepsFactory,

    // Metadata / HMR
    ReplaceMetadata,
    GetReplaceMetadataURL,

    // Injectable / Service
    /// `ɵɵdefineInjectable` — the TS *field name* itself carries the `ɵɵ` prefix.
    DefineInjectableField,
    DeclareInjectable,
    InjectableDeclaration,
    DefineService,
    DeclareService,

    // Component
    DefineComponent,
    DeclareComponent,
    SetComponentScope,
    ChangeDetectionStrategy,
    ViewEncapsulation,
    ComponentDeclaration,

    // Factory
    FactoryDeclaration,
    DeclareFactory,
    FactoryTarget,

    // Directive
    DefineDirective,
    DeclareDirective,
    DirectiveDeclaration,

    // Injector
    InjectorDef,
    InjectorDeclaration,
    DefineInjector,
    DeclareInjector,

    // NgModule
    NgModuleDeclaration,
    ModuleWithProviders,
    DefineNgModule,
    DeclareNgModule,
    SetNgModuleScope,
    RegisterNgModuleType,

    // Pipe defs
    PipeDeclaration,
    DefinePipe,
    DeclarePipe,

    // Class metadata / debug
    DeclareClassMetadata,
    DeclareClassMetadataAsync,
    SetClassMetadata,
    SetClassMetadataAsync,
    SetClassDebugInfo,

    // Queries
    QueryRefresh,
    ViewQuery,
    LoadQuery,
    ContentQuery,
    ViewQuerySignal,
    ContentQuerySignal,
    QueryAdvance,

    // Two-way binding
    TwoWayProperty,
    TwoWayBindingSet,
    TwoWayListener,

    // @let declarations
    DeclareLet,
    StoreLet,
    ReadContextLet,

    // Misc
    ArrowFunction,
    AttachSourceLocations,
    Listener,

    // Features
    NgOnChangesFeature,
    ControlFeature,
    InheritDefinitionFeature,
    ProvidersFeature,
    HostDirectivesFeature,
    ExternalStylesFeature,

    // Sanitization
    SanitizeHtml,
    SanitizeStyle,
    ValidateAttribute,
    SanitizeResourceUrl,
    SanitizeScript,
    SanitizeUrl,
    SanitizeUrlOrResourceUrl,
    TrustConstantHtml,
    TrustConstantResourceUrl,

    // Decorators (no prefix — public API names)
    InputDecorator,
    OutputDecorator,
    ViewChildDecorator,
    ViewChildrenDecorator,
    ContentChildDecorator,
    ContentChildrenDecorator,

    // Type-checking helpers (single ɵ, untyped literals)
    InputSignalBrandWriteType,
    UnwrapDirectiveSignalInputs,
    UnwrapWritableSignal,
    AssertType,
}

impl R3 {
    /// The wire `name` string for this identifier, copied verbatim from the TS
    /// source (preserving `ɵɵ` / `ɵ` / no-prefix exactly). [`R3::Core`] is the
    /// bare namespace import and has no name (`""`).
    pub fn name(self) -> &'static str {
        match self {
            // Namespace
            R3::Core => "",
            R3::NamespaceHTML => "ɵɵnamespaceHTML",
            R3::NamespaceMathML => "ɵɵnamespaceMathML",
            R3::NamespaceSVG => "ɵɵnamespaceSVG",

            // Element / DOM creation
            R3::Element => "ɵɵelement",
            R3::ElementStart => "ɵɵelementStart",
            R3::ElementEnd => "ɵɵelementEnd",
            R3::ForeignComponent => "ɵɵforeignComponent",
            R3::DomElement => "ɵɵdomElement",
            R3::DomElementStart => "ɵɵdomElementStart",
            R3::DomElementEnd => "ɵɵdomElementEnd",
            R3::DomElementContainer => "ɵɵdomElementContainer",
            R3::DomElementContainerStart => "ɵɵdomElementContainerStart",
            R3::DomElementContainerEnd => "ɵɵdomElementContainerEnd",
            R3::DomTemplate => "ɵɵdomTemplate",
            R3::DomListener => "ɵɵdomListener",
            R3::ElementContainerStart => "ɵɵelementContainerStart",
            R3::ElementContainerEnd => "ɵɵelementContainerEnd",
            R3::ElementContainer => "ɵɵelementContainer",

            // Advance / view
            R3::Advance => "ɵɵadvance",
            R3::NextContext => "ɵɵnextContext",
            R3::ResetView => "ɵɵresetView",
            R3::GetCurrentView => "ɵɵgetCurrentView",
            R3::RestoreView => "ɵɵrestoreView",
            R3::Reference => "ɵɵreference",
            R3::EnableBindings => "ɵɵenableBindings",
            R3::DisableBindings => "ɵɵdisableBindings",
            R3::TemplateCreate => "ɵɵtemplate",

            // Property / attribute / style binding
            R3::SyntheticHostProperty => "ɵɵsyntheticHostProperty",
            R3::SyntheticHostListener => "ɵɵsyntheticHostListener",
            R3::Attribute => "ɵɵattribute",
            R3::ClassProp => "ɵɵclassProp",
            R3::StyleMap => "ɵɵstyleMap",
            R3::ClassMap => "ɵɵclassMap",
            R3::StyleProp => "ɵɵstyleProp",
            R3::DomProperty => "ɵɵdomProperty",
            R3::AriaProperty => "ɵɵariaProperty",
            R3::Property => "ɵɵproperty",
            R3::Control => "ɵɵcontrol",
            R3::ControlCreate => "ɵɵcontrolCreate",

            // Interpolation (expression form)
            R3::Interpolate => "ɵɵinterpolate",
            R3::Interpolate1 => "ɵɵinterpolate1",
            R3::Interpolate2 => "ɵɵinterpolate2",
            R3::Interpolate3 => "ɵɵinterpolate3",
            R3::Interpolate4 => "ɵɵinterpolate4",
            R3::Interpolate5 => "ɵɵinterpolate5",
            R3::Interpolate6 => "ɵɵinterpolate6",
            R3::Interpolate7 => "ɵɵinterpolate7",
            R3::Interpolate8 => "ɵɵinterpolate8",
            R3::InterpolateV => "ɵɵinterpolateV",

            // Text + text interpolation
            R3::Text => "ɵɵtext",
            R3::TextInterpolate => "ɵɵtextInterpolate",
            R3::TextInterpolate1 => "ɵɵtextInterpolate1",
            R3::TextInterpolate2 => "ɵɵtextInterpolate2",
            R3::TextInterpolate3 => "ɵɵtextInterpolate3",
            R3::TextInterpolate4 => "ɵɵtextInterpolate4",
            R3::TextInterpolate5 => "ɵɵtextInterpolate5",
            R3::TextInterpolate6 => "ɵɵtextInterpolate6",
            R3::TextInterpolate7 => "ɵɵtextInterpolate7",
            R3::TextInterpolate8 => "ɵɵtextInterpolate8",
            R3::TextInterpolateV => "ɵɵtextInterpolateV",

            // Defer (@defer)
            R3::Defer => "ɵɵdefer",
            R3::DeferWhen => "ɵɵdeferWhen",
            R3::DeferOnIdle => "ɵɵdeferOnIdle",
            R3::DeferOnImmediate => "ɵɵdeferOnImmediate",
            R3::DeferOnTimer => "ɵɵdeferOnTimer",
            R3::DeferOnHover => "ɵɵdeferOnHover",
            R3::DeferOnInteraction => "ɵɵdeferOnInteraction",
            R3::DeferOnViewport => "ɵɵdeferOnViewport",
            R3::DeferPrefetchWhen => "ɵɵdeferPrefetchWhen",
            R3::DeferPrefetchOnIdle => "ɵɵdeferPrefetchOnIdle",
            R3::DeferPrefetchOnImmediate => "ɵɵdeferPrefetchOnImmediate",
            R3::DeferPrefetchOnTimer => "ɵɵdeferPrefetchOnTimer",
            R3::DeferPrefetchOnHover => "ɵɵdeferPrefetchOnHover",
            R3::DeferPrefetchOnInteraction => "ɵɵdeferPrefetchOnInteraction",
            R3::DeferPrefetchOnViewport => "ɵɵdeferPrefetchOnViewport",
            R3::DeferHydrateWhen => "ɵɵdeferHydrateWhen",
            R3::DeferHydrateNever => "ɵɵdeferHydrateNever",
            R3::DeferHydrateOnIdle => "ɵɵdeferHydrateOnIdle",
            R3::DeferHydrateOnImmediate => "ɵɵdeferHydrateOnImmediate",
            R3::DeferHydrateOnTimer => "ɵɵdeferHydrateOnTimer",
            R3::DeferHydrateOnHover => "ɵɵdeferHydrateOnHover",
            R3::DeferHydrateOnInteraction => "ɵɵdeferHydrateOnInteraction",
            R3::DeferHydrateOnViewport => "ɵɵdeferHydrateOnViewport",
            R3::DeferEnableTimerScheduling => "ɵɵdeferEnableTimerScheduling",
            R3::EnableIncrementalHydrationRuntime => "ɵɵenableIncrementalHydrationRuntime",

            // Control flow
            R3::ConditionalCreate => "ɵɵconditionalCreate",
            R3::ConditionalBranchCreate => "ɵɵconditionalBranchCreate",
            R3::Conditional => "ɵɵconditional",
            R3::Repeater => "ɵɵrepeater",
            R3::RepeaterCreate => "ɵɵrepeaterCreate",
            R3::RepeaterTrackByIndex => "ɵɵrepeaterTrackByIndex",
            R3::RepeaterTrackByIdentity => "ɵɵrepeaterTrackByIdentity",
            R3::ComponentInstance => "ɵɵcomponentInstance",

            // Pure functions / pipes
            R3::PureFunction0 => "ɵɵpureFunction0",
            R3::PureFunction1 => "ɵɵpureFunction1",
            R3::PureFunction2 => "ɵɵpureFunction2",
            R3::PureFunction3 => "ɵɵpureFunction3",
            R3::PureFunction4 => "ɵɵpureFunction4",
            R3::PureFunction5 => "ɵɵpureFunction5",
            R3::PureFunction6 => "ɵɵpureFunction6",
            R3::PureFunction7 => "ɵɵpureFunction7",
            R3::PureFunction8 => "ɵɵpureFunction8",
            R3::PureFunctionV => "ɵɵpureFunctionV",
            R3::PipeBind1 => "ɵɵpipeBind1",
            R3::PipeBind2 => "ɵɵpipeBind2",
            R3::PipeBind3 => "ɵɵpipeBind3",
            R3::PipeBind4 => "ɵɵpipeBind4",
            R3::PipeBindV => "ɵɵpipeBindV",
            R3::Pipe => "ɵɵpipe",

            // Animations
            R3::AnimationEnterListener => "ɵɵanimateEnterListener",
            R3::AnimationLeaveListener => "ɵɵanimateLeaveListener",
            R3::AnimationEnter => "ɵɵanimateEnter",
            R3::AnimationLeave => "ɵɵanimateLeave",

            // i18n
            R3::I18n => "ɵɵi18n",
            R3::I18nAttributes => "ɵɵi18nAttributes",
            R3::I18nExp => "ɵɵi18nExp",
            R3::I18nStart => "ɵɵi18nStart",
            R3::I18nEnd => "ɵɵi18nEnd",
            R3::I18nApply => "ɵɵi18nApply",
            R3::I18nPostprocess => "ɵɵi18nPostprocess",

            // Projection / content
            R3::Projection => "ɵɵprojection",
            R3::ProjectionDef => "ɵɵprojectionDef",

            // Dependency injection
            R3::Inject => "ɵɵinject",
            R3::InjectAttribute => "ɵɵinjectAttribute",
            R3::DirectiveInject => "ɵɵdirectiveInject",
            R3::InvalidFactory => "ɵɵinvalidFactory",
            R3::InvalidFactoryDep => "ɵɵinvalidFactoryDep",
            R3::TemplateRefExtractor => "ɵɵtemplateRefExtractor",
            R3::ForwardRef => "forwardRef",
            R3::ResolveForwardRef => "resolveForwardRef",
            R3::GetInheritedFactory => "ɵɵgetInheritedFactory",
            R3::ResolveWindow => "ɵɵresolveWindow",
            R3::ResolveDocument => "ɵɵresolveDocument",
            R3::ResolveBody => "ɵɵresolveBody",
            R3::GetComponentDepsFactory => "ɵɵgetComponentDepsFactory",

            // Metadata / HMR
            R3::ReplaceMetadata => "ɵɵreplaceMetadata",
            R3::GetReplaceMetadataURL => "ɵɵgetReplaceMetadataURL",

            // Injectable / Service
            R3::DefineInjectableField => "ɵɵdefineInjectable",
            R3::DeclareInjectable => "ɵɵngDeclareInjectable",
            R3::InjectableDeclaration => "ɵɵInjectableDeclaration",
            R3::DefineService => "ɵɵdefineService",
            R3::DeclareService => "ɵɵngDeclareService",

            // Component
            R3::DefineComponent => "ɵɵdefineComponent",
            R3::DeclareComponent => "ɵɵngDeclareComponent",
            R3::SetComponentScope => "ɵɵsetComponentScope",
            R3::ChangeDetectionStrategy => "ChangeDetectionStrategy",
            R3::ViewEncapsulation => "ViewEncapsulation",
            R3::ComponentDeclaration => "ɵɵComponentDeclaration",

            // Factory
            R3::FactoryDeclaration => "ɵɵFactoryDeclaration",
            R3::DeclareFactory => "ɵɵngDeclareFactory",
            R3::FactoryTarget => "ɵɵFactoryTarget",

            // Directive
            R3::DefineDirective => "ɵɵdefineDirective",
            R3::DeclareDirective => "ɵɵngDeclareDirective",
            R3::DirectiveDeclaration => "ɵɵDirectiveDeclaration",

            // Injector
            R3::InjectorDef => "ɵɵInjectorDef",
            R3::InjectorDeclaration => "ɵɵInjectorDeclaration",
            R3::DefineInjector => "ɵɵdefineInjector",
            R3::DeclareInjector => "ɵɵngDeclareInjector",

            // NgModule
            R3::NgModuleDeclaration => "ɵɵNgModuleDeclaration",
            R3::ModuleWithProviders => "ModuleWithProviders",
            R3::DefineNgModule => "ɵɵdefineNgModule",
            R3::DeclareNgModule => "ɵɵngDeclareNgModule",
            R3::SetNgModuleScope => "ɵɵsetNgModuleScope",
            R3::RegisterNgModuleType => "ɵɵregisterNgModuleType",

            // Pipe defs
            R3::PipeDeclaration => "ɵɵPipeDeclaration",
            R3::DefinePipe => "ɵɵdefinePipe",
            R3::DeclarePipe => "ɵɵngDeclarePipe",

            // Class metadata / debug
            R3::DeclareClassMetadata => "ɵɵngDeclareClassMetadata",
            R3::DeclareClassMetadataAsync => "ɵɵngDeclareClassMetadataAsync",
            R3::SetClassMetadata => "ɵsetClassMetadata",
            R3::SetClassMetadataAsync => "ɵsetClassMetadataAsync",
            R3::SetClassDebugInfo => "ɵsetClassDebugInfo",

            // Queries
            R3::QueryRefresh => "ɵɵqueryRefresh",
            R3::ViewQuery => "ɵɵviewQuery",
            R3::LoadQuery => "ɵɵloadQuery",
            R3::ContentQuery => "ɵɵcontentQuery",
            R3::ViewQuerySignal => "ɵɵviewQuerySignal",
            R3::ContentQuerySignal => "ɵɵcontentQuerySignal",
            R3::QueryAdvance => "ɵɵqueryAdvance",

            // Two-way binding
            R3::TwoWayProperty => "ɵɵtwoWayProperty",
            R3::TwoWayBindingSet => "ɵɵtwoWayBindingSet",
            R3::TwoWayListener => "ɵɵtwoWayListener",

            // @let declarations
            R3::DeclareLet => "ɵɵdeclareLet",
            R3::StoreLet => "ɵɵstoreLet",
            R3::ReadContextLet => "ɵɵreadContextLet",

            // Misc
            R3::ArrowFunction => "ɵɵarrowFunction",
            R3::AttachSourceLocations => "ɵɵattachSourceLocations",
            R3::Listener => "ɵɵlistener",

            // Features
            R3::NgOnChangesFeature => "ɵɵNgOnChangesFeature",
            R3::ControlFeature => "ɵɵControlFeature",
            R3::InheritDefinitionFeature => "ɵɵInheritDefinitionFeature",
            R3::ProvidersFeature => "ɵɵProvidersFeature",
            R3::HostDirectivesFeature => "ɵɵHostDirectivesFeature",
            R3::ExternalStylesFeature => "ɵɵExternalStylesFeature",

            // Sanitization
            R3::SanitizeHtml => "ɵɵsanitizeHtml",
            R3::SanitizeStyle => "ɵɵsanitizeStyle",
            R3::ValidateAttribute => "ɵɵvalidateAttribute",
            R3::SanitizeResourceUrl => "ɵɵsanitizeResourceUrl",
            R3::SanitizeScript => "ɵɵsanitizeScript",
            R3::SanitizeUrl => "ɵɵsanitizeUrl",
            R3::SanitizeUrlOrResourceUrl => "ɵɵsanitizeUrlOrResourceUrl",
            R3::TrustConstantHtml => "ɵɵtrustConstantHtml",
            R3::TrustConstantResourceUrl => "ɵɵtrustConstantResourceUrl",

            // Decorators
            R3::InputDecorator => "Input",
            R3::OutputDecorator => "Output",
            R3::ViewChildDecorator => "ViewChild",
            R3::ViewChildrenDecorator => "ViewChildren",
            R3::ContentChildDecorator => "ContentChild",
            R3::ContentChildrenDecorator => "ContentChildren",

            // Type-checking helpers
            R3::InputSignalBrandWriteType => "ɵINPUT_SIGNAL_BRAND_WRITE_TYPE",
            R3::UnwrapDirectiveSignalInputs => "ɵUnwrapDirectiveSignalInputs",
            R3::UnwrapWritableSignal => "ɵunwrapWritableSignal",
            R3::AssertType => "ɵassertType",
        }
    }

    /// The owned [`ExternalReference`] for this identifier (`{name, module_name:
    /// Some("@angular/core")}`). Equivalent to reading a static field of
    /// Angular's `Identifiers` class.
    pub fn reference(self) -> ExternalReference {
        core_ref(self.name())
    }
}

// -------------------------------------------------------------------------
// Free-function accessors (ergonomic alternative to `R3::Element.reference()`).
//
// These mirror the TS static-field names directly. Each returns a freshly
// owned `ExternalReference`. They cover the most commonly referenced
// instructions; for the complete set use the `R3` enum + `reference()`.
// -------------------------------------------------------------------------

/// The bare `@angular/core` namespace import (`Identifiers.core`); name is empty.
pub fn core() -> ExternalReference {
    R3::Core.reference()
}
pub fn element() -> ExternalReference {
    R3::Element.reference()
}
pub fn element_start() -> ExternalReference {
    R3::ElementStart.reference()
}
pub fn element_end() -> ExternalReference {
    R3::ElementEnd.reference()
}
pub fn text() -> ExternalReference {
    R3::Text.reference()
}
pub fn text_interpolate() -> ExternalReference {
    R3::TextInterpolate.reference()
}
pub fn property() -> ExternalReference {
    R3::Property.reference()
}
pub fn advance() -> ExternalReference {
    R3::Advance.reference()
}
pub fn listener() -> ExternalReference {
    R3::Listener.reference()
}
pub fn template_create() -> ExternalReference {
    R3::TemplateCreate.reference()
}
pub fn define_component() -> ExternalReference {
    R3::DefineComponent.reference()
}
pub fn define_directive() -> ExternalReference {
    R3::DefineDirective.reference()
}
pub fn define_pipe() -> ExternalReference {
    R3::DefinePipe.reference()
}
pub fn define_ng_module() -> ExternalReference {
    R3::DefineNgModule.reference()
}
pub fn define_injector() -> ExternalReference {
    R3::DefineInjector.reference()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full list of variants, in declaration order, used to exercise every
    /// `name()` arm and to assert the table size matches Angular's 214 members.
    const ALL: &[R3] = &[
        R3::Core,
        R3::NamespaceHTML,
        R3::NamespaceMathML,
        R3::NamespaceSVG,
        R3::Element,
        R3::ElementStart,
        R3::ElementEnd,
        R3::ForeignComponent,
        R3::DomElement,
        R3::DomElementStart,
        R3::DomElementEnd,
        R3::DomElementContainer,
        R3::DomElementContainerStart,
        R3::DomElementContainerEnd,
        R3::DomTemplate,
        R3::DomListener,
        R3::ElementContainerStart,
        R3::ElementContainerEnd,
        R3::ElementContainer,
        R3::Advance,
        R3::NextContext,
        R3::ResetView,
        R3::GetCurrentView,
        R3::RestoreView,
        R3::Reference,
        R3::EnableBindings,
        R3::DisableBindings,
        R3::TemplateCreate,
        R3::SyntheticHostProperty,
        R3::SyntheticHostListener,
        R3::Attribute,
        R3::ClassProp,
        R3::StyleMap,
        R3::ClassMap,
        R3::StyleProp,
        R3::DomProperty,
        R3::AriaProperty,
        R3::Property,
        R3::Control,
        R3::ControlCreate,
        R3::Interpolate,
        R3::Interpolate1,
        R3::Interpolate2,
        R3::Interpolate3,
        R3::Interpolate4,
        R3::Interpolate5,
        R3::Interpolate6,
        R3::Interpolate7,
        R3::Interpolate8,
        R3::InterpolateV,
        R3::Text,
        R3::TextInterpolate,
        R3::TextInterpolate1,
        R3::TextInterpolate2,
        R3::TextInterpolate3,
        R3::TextInterpolate4,
        R3::TextInterpolate5,
        R3::TextInterpolate6,
        R3::TextInterpolate7,
        R3::TextInterpolate8,
        R3::TextInterpolateV,
        R3::Defer,
        R3::DeferWhen,
        R3::DeferOnIdle,
        R3::DeferOnImmediate,
        R3::DeferOnTimer,
        R3::DeferOnHover,
        R3::DeferOnInteraction,
        R3::DeferOnViewport,
        R3::DeferPrefetchWhen,
        R3::DeferPrefetchOnIdle,
        R3::DeferPrefetchOnImmediate,
        R3::DeferPrefetchOnTimer,
        R3::DeferPrefetchOnHover,
        R3::DeferPrefetchOnInteraction,
        R3::DeferPrefetchOnViewport,
        R3::DeferHydrateWhen,
        R3::DeferHydrateNever,
        R3::DeferHydrateOnIdle,
        R3::DeferHydrateOnImmediate,
        R3::DeferHydrateOnTimer,
        R3::DeferHydrateOnHover,
        R3::DeferHydrateOnInteraction,
        R3::DeferHydrateOnViewport,
        R3::DeferEnableTimerScheduling,
        R3::EnableIncrementalHydrationRuntime,
        R3::ConditionalCreate,
        R3::ConditionalBranchCreate,
        R3::Conditional,
        R3::Repeater,
        R3::RepeaterCreate,
        R3::RepeaterTrackByIndex,
        R3::RepeaterTrackByIdentity,
        R3::ComponentInstance,
        R3::PureFunction0,
        R3::PureFunction1,
        R3::PureFunction2,
        R3::PureFunction3,
        R3::PureFunction4,
        R3::PureFunction5,
        R3::PureFunction6,
        R3::PureFunction7,
        R3::PureFunction8,
        R3::PureFunctionV,
        R3::PipeBind1,
        R3::PipeBind2,
        R3::PipeBind3,
        R3::PipeBind4,
        R3::PipeBindV,
        R3::Pipe,
        R3::AnimationEnterListener,
        R3::AnimationLeaveListener,
        R3::AnimationEnter,
        R3::AnimationLeave,
        R3::I18n,
        R3::I18nAttributes,
        R3::I18nExp,
        R3::I18nStart,
        R3::I18nEnd,
        R3::I18nApply,
        R3::I18nPostprocess,
        R3::Projection,
        R3::ProjectionDef,
        R3::Inject,
        R3::InjectAttribute,
        R3::DirectiveInject,
        R3::InvalidFactory,
        R3::InvalidFactoryDep,
        R3::TemplateRefExtractor,
        R3::ForwardRef,
        R3::ResolveForwardRef,
        R3::GetInheritedFactory,
        R3::ResolveWindow,
        R3::ResolveDocument,
        R3::ResolveBody,
        R3::GetComponentDepsFactory,
        R3::ReplaceMetadata,
        R3::GetReplaceMetadataURL,
        R3::DefineInjectableField,
        R3::DeclareInjectable,
        R3::InjectableDeclaration,
        R3::DefineService,
        R3::DeclareService,
        R3::DefineComponent,
        R3::DeclareComponent,
        R3::SetComponentScope,
        R3::ChangeDetectionStrategy,
        R3::ViewEncapsulation,
        R3::ComponentDeclaration,
        R3::FactoryDeclaration,
        R3::DeclareFactory,
        R3::FactoryTarget,
        R3::DefineDirective,
        R3::DeclareDirective,
        R3::DirectiveDeclaration,
        R3::InjectorDef,
        R3::InjectorDeclaration,
        R3::DefineInjector,
        R3::DeclareInjector,
        R3::NgModuleDeclaration,
        R3::ModuleWithProviders,
        R3::DefineNgModule,
        R3::DeclareNgModule,
        R3::SetNgModuleScope,
        R3::RegisterNgModuleType,
        R3::PipeDeclaration,
        R3::DefinePipe,
        R3::DeclarePipe,
        R3::DeclareClassMetadata,
        R3::DeclareClassMetadataAsync,
        R3::SetClassMetadata,
        R3::SetClassMetadataAsync,
        R3::SetClassDebugInfo,
        R3::QueryRefresh,
        R3::ViewQuery,
        R3::LoadQuery,
        R3::ContentQuery,
        R3::ViewQuerySignal,
        R3::ContentQuerySignal,
        R3::QueryAdvance,
        R3::TwoWayProperty,
        R3::TwoWayBindingSet,
        R3::TwoWayListener,
        R3::DeclareLet,
        R3::StoreLet,
        R3::ReadContextLet,
        R3::ArrowFunction,
        R3::AttachSourceLocations,
        R3::Listener,
        R3::NgOnChangesFeature,
        R3::ControlFeature,
        R3::InheritDefinitionFeature,
        R3::ProvidersFeature,
        R3::HostDirectivesFeature,
        R3::ExternalStylesFeature,
        R3::SanitizeHtml,
        R3::SanitizeStyle,
        R3::ValidateAttribute,
        R3::SanitizeResourceUrl,
        R3::SanitizeScript,
        R3::SanitizeUrl,
        R3::SanitizeUrlOrResourceUrl,
        R3::TrustConstantHtml,
        R3::TrustConstantResourceUrl,
        R3::InputDecorator,
        R3::OutputDecorator,
        R3::ViewChildDecorator,
        R3::ViewChildrenDecorator,
        R3::ContentChildDecorator,
        R3::ContentChildrenDecorator,
        R3::InputSignalBrandWriteType,
        R3::UnwrapDirectiveSignalInputs,
        R3::UnwrapWritableSignal,
        R3::AssertType,
    ];

    #[test]
    fn table_has_214_members() {
        // Matches Angular 22.1.0-next.0's `Identifiers` static-member count.
        assert_eq!(ALL.len(), 214);
    }

    #[test]
    fn core_has_no_name_and_core_module() {
        let r = R3::Core.reference();
        assert_eq!(r.name, "");
        assert_eq!(r.module_name.as_deref(), Some("@angular/core"));
    }

    #[test]
    fn every_reference_targets_core() {
        for &id in ALL {
            let r = id.reference();
            assert_eq!(
                r.module_name.as_deref(),
                Some("@angular/core"),
                "{id:?} should resolve against @angular/core",
            );
        }
    }

    #[test]
    fn names_are_unique() {
        // Every variant except Core should have a distinct, non-empty wire name.
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for &id in ALL {
            let name = id.name();
            if id == R3::Core {
                assert_eq!(name, "");
                continue;
            }
            assert!(!name.is_empty(), "{id:?} has empty name");
            assert!(seen.insert(name), "duplicate wire name {name:?} for {id:?}");
        }
    }

    #[test]
    fn field_name_diverges_from_wire_name() {
        // templateCreate -> ɵɵtemplate; the animation* family -> ɵɵanimate*.
        assert_eq!(R3::TemplateCreate.name(), "ɵɵtemplate");
        assert_eq!(R3::AnimationEnterListener.name(), "ɵɵanimateEnterListener");
        assert_eq!(R3::AnimationLeaveListener.name(), "ɵɵanimateLeaveListener");
        assert_eq!(R3::AnimationEnter.name(), "ɵɵanimateEnter");
        assert_eq!(R3::AnimationLeave.name(), "ɵɵanimateLeave");
    }

    #[test]
    fn prefix_families_preserved() {
        // double ɵɵ
        assert!(R3::Element.name().starts_with("ɵɵ"));
        // single ɵ (not double)
        assert!(R3::SetClassMetadata.name().starts_with('ɵ'));
        assert!(!R3::SetClassMetadata.name().starts_with("ɵɵ"));
        assert!(R3::AssertType.name().starts_with('ɵ'));
        assert!(!R3::AssertType.name().starts_with("ɵɵ"));
        // no prefix (public API)
        assert_eq!(R3::ForwardRef.name(), "forwardRef");
        assert_eq!(R3::ChangeDetectionStrategy.name(), "ChangeDetectionStrategy");
        assert_eq!(R3::InputDecorator.name(), "Input");
    }

    #[test]
    fn free_fn_accessors_match_enum() {
        assert_eq!(element(), R3::Element.reference());
        assert_eq!(text_interpolate(), R3::TextInterpolate.reference());
        assert_eq!(define_component().name, "ɵɵdefineComponent");
        assert_eq!(template_create().name, "ɵɵtemplate");
    }
}
