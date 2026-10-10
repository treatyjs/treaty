//! Plain-data decorator FACTS: what a decorated class declares, independent of which extractor
//! read it.
//!
//! Both extractors produce these types: the bridge (`adapt`, from ng-analyze) and Treaty's own
//! front-end (`treaty_ivy::decorator_facts`, from the parse IR). The parity test diffs the two, and
//! the facade's feature-gated emit path consumes the bridge's copy.
//!
//! Conventions, applied identically by both producers:
//!   * Facts are RAW: they record what the author wrote (the `signals` option as written, the
//!     selector as written, a member's read as written). Treaty conventions are applied afterwards by
//!     the consumer (the filename-derived default selector, `signals` = explicit OR member-derived,
//!     the modernizer options, pipe `pure` defaulting to true). Keeping them out of the facts is what
//!     lets the two extractors be compared field by field.
//!   * Every expression-valued field is an [`ExprFact`] holding JavaScript source TEXT. The bridge
//!     copies it byte-exactly from the source; Treaty's extractor prints its lowered form. The parity
//!     normaliser lowers both sides through the same path before comparing.
//!   * No oxc or ng-analyze type appears here.

/// The Angular decorator a class carries (the first recognised one wins; see `DecoratorFacts::kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClassKind {
    Component,
    Directive,
    Pipe,
    Injectable,
    NgModule,
}

/// An expression-valued fact: its JavaScript source text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExprFact {
    pub text: String,
}

impl ExprFact {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Everything one decorated class declares. Exactly one of the per-kind sections is populated for
/// the class's `kind`; `directive` is also populated for a component (a component is a directive
/// plus component-only fields).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DecoratorFacts {
    pub class_name: Option<String>,
    /// Byte span of the class declaration.
    pub class_start: usize,
    /// `None` when the class carries no recognised Angular decorator.
    pub kind: Option<ClassKind>,
    pub directive: Option<DirectiveFacts>,
    pub component: Option<ComponentFacts>,
    pub pipe: Option<PipeFacts>,
    pub injectable: Option<InjectableFacts>,
    pub ng_module: Option<NgModuleFacts>,
    /// Constructor dependencies, in parameter order. `None` when the class has no constructor.
    pub constructor_deps: Option<Vec<DepFact>>,
    /// Why this extractor refuses the class (empty when it extracted it). The bridge fills this when
    /// ng-analyze reports a diagnostic inside the class or the file cannot be analysed; Treaty's
    /// extractor fills it with the error it would return.
    pub unsupported: Vec<String>,
}

/// Facts shared by `@Component` and `@Directive`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirectiveFacts {
    /// The `selector` as written (`None` when absent).
    pub selector: Option<String>,
    /// The `standalone` option (`true` when absent, the Angular v19+ default).
    pub standalone: bool,
    /// The explicit `signals` option only. Member-derived signal detection is a consumer convention.
    pub signals: bool,
    /// `exportAs` split on commas and trimmed, in declared order.
    pub export_as: Option<Vec<String>>,
    /// The `host: {...}` object's entries in source order (`None` when the key is absent).
    pub host_object: Option<Vec<(String, HostValue)>>,
    /// `@HostBinding` members, in declaration order.
    pub host_bindings: Vec<HostBindingFact>,
    /// `@HostListener` members, in declaration order.
    pub host_listeners: Vec<HostListenerFact>,
    pub host_directives: Option<Vec<HostDirectiveFact>>,
    pub providers: Option<ExprFact>,
    /// Inputs (decorator and signal), in declaration order.
    pub inputs: Vec<InputFact>,
    /// Outputs (decorator, signal and the `model()` change output), in declaration order.
    pub outputs: Vec<OutputFact>,
    /// Content queries (`@ContentChild*`, `contentChild*()`), in declaration order.
    pub content_queries: Vec<QueryFact>,
    /// View queries (`@ViewChild*`, `viewChild*()`), in declaration order.
    pub view_queries: Vec<QueryFact>,
    /// The class declares an `ngOnChanges` method.
    pub uses_on_changes: bool,
}

/// A `host` object value: a string literal, or a non-literal expression's source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostValue {
    Str(String),
    Expr(String),
}

/// A `@HostBinding('hostProp') member` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBindingFact {
    /// The host property (the decorator argument, or the member name when absent).
    pub host_prop: String,
    /// The class member the binding reads, as declared.
    pub member: String,
    /// Byte offset of the member declaration. Host entries from members are ordered by it, interleaved
    /// with `HostListenerFact`s, exactly as the author declared them.
    pub member_start: usize,
}

/// A `@HostListener('event', [args]) method(...)` declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostListenerFact {
    pub event: String,
    pub method: String,
    /// The argument-name strings from the decorator's second argument, in order.
    pub args: Vec<String>,
    /// Byte offset of the member declaration (see [`HostBindingFact::member_start`]).
    pub member_start: usize,
}

/// One `hostDirectives` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostDirectiveFact {
    /// The directive's identifier, as written (`Dir`, or `ns.Dir` for a namespace member).
    pub name: String,
    /// Whether it was written as `forwardRef(() => Dir)`.
    pub forward_ref: bool,
    /// `inputs` mapping as (public name, alias) pairs, in declared order.
    pub inputs: Option<Vec<(String, String)>>,
    pub outputs: Option<Vec<(String, String)>>,
}

/// An input: a decorated `@Input` or a signal `input()` / `model()` member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputFact {
    /// The class property name.
    pub property: String,
    /// The public (binding) name; equals `property` when no alias is declared.
    pub public_name: String,
    pub required: bool,
    /// True for a signal input (`input()` / `model()`), false for a decorator input.
    pub is_signal: bool,
    /// True when declared with an `@Input` decorator (the modernizer may lower those to signal inputs).
    pub from_decorator: bool,
    /// The `transform` function, when declared.
    pub transform: Option<ExprFact>,
}

/// An output: a decorated `@Output` or a signal `output()` / `outputFromObservable()` / `model()` change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputFact {
    pub property: String,
    pub public_name: String,
}

/// The predicate of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryPredicateFact {
    /// A string locator split on commas and trimmed (or an array of strings).
    Selectors(Vec<String>),
    /// A type or token expression.
    Expr(ExprFact),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryFact {
    pub property: String,
    /// `viewChild` / `contentChild` / `@ViewChild` / `@ContentChild` (a single result).
    pub first: bool,
    /// True for content queries, false for view queries (mirrors which list the query sits in).
    pub is_content: bool,
    pub predicate: QueryPredicateFact,
    pub descendants: bool,
    pub read: Option<ExprFact>,
    /// The decorator `static` option (single-result decorator queries only).
    pub is_static: bool,
    pub is_signal: bool,
}

/// Component-only fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ComponentFacts {
    /// Inline `template` string (`None` when absent or when `templateUrl` is used).
    pub template: Option<String>,
    /// `templateUrl` as written.
    pub template_url: Option<String>,
    /// Inline `styles` strings, in order.
    pub styles: Vec<String>,
    /// `styleUrls` (and `styleUrl`) as written, in order.
    pub style_urls: Vec<String>,
    /// The `changeDetection` member name as written (`OnPush`, `Default`, ...), `None` when absent or
    /// not a member expression. Absent and an explicit `OnPush` are both reported as `None` by
    /// ng-analyze, so consumers must treat `None` as the Angular default.
    pub change_detection: Option<String>,
    /// `encapsulation` as its numeric `ViewEncapsulation` value.
    pub encapsulation: Option<i32>,
    pub preserve_whitespaces: Option<bool>,
    pub animations: Option<ExprFact>,
    pub view_providers: Option<ExprFact>,
    /// Identifiers in the `imports` array (`None` when absent).
    pub imports: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PipeFacts {
    pub name: Option<String>,
    /// The `pure` option as written (`None` when absent; the consumer defaults it to true).
    pub pure: Option<bool>,
    /// The `standalone` option as written (`None` when absent).
    pub standalone: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InjectableFacts {
    /// `providedIn` as its source text (`'root'`, `Foo`, ...).
    pub provided_in: Option<ExprFact>,
    pub use_class: Option<ExprFact>,
    pub use_factory: Option<ExprFact>,
    pub use_value: Option<ExprFact>,
    pub use_existing: Option<ExprFact>,
    /// The `deps` array, when declared.
    pub deps: Option<Vec<DepFact>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NgModuleFacts {
    pub declarations: Option<Vec<String>>,
    pub imports: Option<Vec<String>>,
    pub exports: Option<Vec<String>>,
    pub bootstrap: Option<Vec<String>>,
    pub providers: Option<ExprFact>,
    pub schemas: Option<Vec<String>>,
}

/// One dependency: a constructor parameter or an `InjectableDef.deps` entry.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DepFact {
    /// The token expression as source text (`Foo`, the `@Inject(...)` argument, ...). `None` when the
    /// token is not recoverable.
    pub token: Option<ExprFact>,
    /// `@Attribute('name')` / `attribute` dependency.
    pub attribute: Option<String>,
    pub optional: bool,
    pub host: bool,
    pub self_: bool,
    pub skip_self: bool,
}
