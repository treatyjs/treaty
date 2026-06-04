//! JSX authoring front-end: compile a `.tsx` / `.tjsx` component source into an Angular Ivy
//! component, reusing the shared render3 backend ([`crate::sfc::compile_from_parts`]).
//!
//! Strategy (deliberately NOT a new template IR): a JSX component is a function that returns a JSX
//! tree. We parse the source with OXC (JSX + TypeScript), locate the component function, lower its
//! returned JSX into an Angular template HTML string ([`template`]), strip that return out of the
//! JavaScript body, assemble a component-body JS chunk, lift any `server { … }` block
//! ([`crate::plugin::extract_server_block`]), then hand the parts to the same Ivy codegen the
//! `.treaty` path uses.
//!
//! Full element/attribute lowering lives in [`template`] (and the name-level policy in
//! [`directives`]): HTML/component tags, fragments, text + `{expr}` interpolation, property
//! bindings, `class`/`style` string/object/array forms, `onX` event handlers, and `{...spread}`.
//! The [`control_flow`] and [`signals`] submodules (JSX `&&`/`.map`/ternary → `@if`/`@for`, and
//! hook-style signal lowering) are filled in by later phases.

pub mod angular_blocks;
pub mod control_flow;
pub mod directives;
pub mod react;
pub mod signals;
pub mod template;
pub mod ts_erase;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    ArrowFunctionExpression, Declaration, ExportDefaultDeclarationKind, Expression, Function,
    Statement,
};
use oxc_parser::Parser as JsParser;
use oxc_span::SourceType;

use crate::plugin::{
    client_runtime_imports_for_code, extract_server_block_jsx, rewrite_call_sites, PluginRegistry,
};
use crate::sfc::compile_from_parts_with_directives_and_map;
use crate::source_map::redact_server_bodies_in_map;
use crate::CompiledAuthoring;

/// PascalCase the stem of a file name, reused as the component class name. Mirrors the `.treaty`
/// derivation (path separators, the authoring extension, and a trailing `.component` segment are all
/// dropped) so JSX and `.treaty` components name themselves the same way — and so the class name and
/// the kebab-case selector (derived from the same stem by `crate::sfc`) stay aligned.
fn to_pascal_case(file_name: &str) -> String {
    let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
    // Drop the authoring extension (the final `.ext`), then a trailing `.component` segment.
    let no_ext = base.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(base);
    let stem = no_ext.strip_suffix(".component").unwrap_or(no_ext);

    let mut out = String::new();
    let mut new_word = true;
    for ch in stem.chars() {
        if ch.is_ascii_alphanumeric() {
            if new_word {
                out.extend(ch.to_uppercase());
                new_word = false;
            } else {
                out.push(ch);
            }
        } else {
            new_word = true;
        }
    }

    if out.is_empty() {
        "JsxComponent".to_string()
    } else {
        out
    }
}

/// Collect the imported binding names from an already-parsed program — the selectorless auto-import
/// candidate set (every default / namespace / named import local name). Parsing here is the
/// JSX-aware parse the front-end already performed, so JSX in the body does not interfere.
fn collect_import_names(body: &[Statement]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for stmt in body {
        let Statement::ImportDeclaration(import) = stmt else {
            continue;
        };
        let Some(specifiers) = &import.specifiers else {
            continue;
        };
        for spec in specifiers {
            use oxc_ast::ast::ImportDeclarationSpecifier::*;
            match spec {
                ImportSpecifier(s) => names.push(s.local.name.to_string()),
                ImportDefaultSpecifier(s) => names.push(s.local.name.to_string()),
                ImportNamespaceSpecifier(s) => names.push(s.local.name.to_string()),
            }
        }
    }
    names
}

/// Collect the names declared by a single (possibly `export`-wrapped) declaration into `out`: a
/// `function`/`class` declaration contributes its id, and a `const`/`let`/`var` declaration each of
/// its bound identifier names (a simple `const x = …` binding; destructuring patterns carry no
/// single directive identifier and are skipped). Shared by [`collect_local_declaration_names`] for
/// both bare and `export`-prefixed declarations.
fn collect_declaration_names(decl: &Declaration, out: &mut Vec<String>) {
    match decl {
        Declaration::FunctionDeclaration(func) => {
            if let Some(id) = &func.id {
                out.push(id.name.to_string());
            }
        }
        Declaration::ClassDeclaration(class) => {
            if let Some(id) = &class.id {
                out.push(id.name.to_string());
            }
        }
        Declaration::VariableDeclaration(var) => {
            for declarator in &var.declarations {
                if let Some(name) = declarator.id.get_identifier_name() {
                    out.push(name.to_string());
                }
            }
        }
        _ => {}
    }
}

/// Collect the names of every top-level local declaration in the module: bare `function`/`class`/
/// `const` declarations AND those wrapped in an `export` (`export function highlight()`,
/// `export const Tooltip = …`). These are the IN-SCOPE symbols — alongside the imported names — that
/// a `use:`/capitalized/structural directive reference can resolve to. A directive declared locally
/// (e.g. `counter.tsx`'s `export function highlight()`) survives into the emitted module via
/// [`assemble_flat_body`], so its name is a valid dependency target; collecting it here lets the
/// directive lowering reference the REAL identifier (`highlight`) instead of a fabricated `Highlight`.
fn collect_local_declaration_names(body: &[Statement]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for stmt in body {
        match stmt {
            Statement::FunctionDeclaration(func) => {
                if let Some(id) = &func.id {
                    names.push(id.name.to_string());
                }
            }
            Statement::ClassDeclaration(class) => {
                if let Some(id) = &class.id {
                    names.push(id.name.to_string());
                }
            }
            Statement::VariableDeclaration(var) => {
                for declarator in &var.declarations {
                    if let Some(name) = declarator.id.get_identifier_name() {
                        names.push(name.to_string());
                    }
                }
            }
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    collect_declaration_names(decl, &mut names);
                }
            }
            Statement::ExportDefaultDeclaration(export) => {
                // A named default-export declaration (`export default function highlight() {}`)
                // also contributes its id as an in-scope symbol.
                match &export.declaration {
                    ExportDefaultDeclarationKind::FunctionDeclaration(func) => {
                        if let Some(id) = &func.id {
                            names.push(id.name.to_string());
                        }
                    }
                    ExportDefaultDeclarationKind::ClassDeclaration(class) => {
                        if let Some(id) = &class.id {
                            names.push(id.name.to_string());
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    names
}

/// The located component and its lowered template.
///
/// The JSX front-end deliberately does NOT splice the author's source verbatim into the module: the
/// author writes `export default function App() { …body…; return <JSX/> }` (or a named-function /
/// arrow form), and the shared backend ([`crate::sfc::build_module`]) re-wraps a *flat* component
/// body inside its own `function {Class}() { … return { bindings }; }` envelope. Leaving the
/// author's enclosing `export default function`/`const X = () =>` in the body produces an illegal
/// nested export and stray braces (the malformed-module bug). So we lower the component to its
/// FLAT inner body here — the author's function/arrow envelope and its JSX `return` are dropped,
/// exactly as the `.treaty` path hands the backend a flat declaration list.
struct LoweredComponent {
    /// The Angular template HTML lowered from the component's returned JSX.
    template_html: String,
    /// The byte span of the WHOLE component declaration statement in the (preprocessed) source —
    /// the `export default function App(){…}` / `function App(){…}` / `const App = () => …;` /
    /// `export default App;` etc. This entire region is removed from the module-level body and
    /// replaced by [`Self::component_body`], so no `export default`/function envelope survives.
    declaration_span: (usize, usize),
    /// The flattened component-body JavaScript: the statements *inside* the component function/arrow
    /// body, with the JSX `return`/expression-body sliced out (the lowered template is the single
    /// source of truth for markup). This is module-top-level shaped — the same flat form the
    /// `.treaty` path produces — so the shared backend's `build_module` and the signals pass both
    /// treat its declarations as component state.
    component_body: String,
    /// The author's name for the component, when it has one (a named `function App` or a
    /// `const App = …`). Used to drop a sibling bare `export default App;` statement that re-exports
    /// the component by name — the backend emits its own `export default {Class};`. A bare default
    /// export (`export default function () {}` / `export default () => …`) has no name and is `None`.
    name: Option<String>,
    /// The prop names lowered from the component's destructured props parameter to signal `input()`s
    /// (e.g. `function C({ a, b = 5 })` → `const a = input(); const b = input(5);`). Recorded so the
    /// template auto-calls a bare `{{ a }}` prop read just like any other signal.
    prop_signals: Vec<String>,
    /// Non-fatal diagnostics from props lowering (e.g. a non-destructured `(props)` parameter that
    /// cannot be enumerated into discrete inputs).
    prop_diagnostics: Vec<String>,
}

/// Compile a JSX component source into an Angular Ivy component.
///
/// `file_name` derives the component class name (PascalCase of the stem). The component is
/// standalone and selectorless, exactly like the `.treaty` path. A `server { … }` block is lifted
/// and routed through the reference Elysia/Eden backend, mirroring `sfc::compile_treaty_authoring`.
pub fn compile(source: &str, file_name: &str) -> CompiledAuthoring {
    // 1. Lift any server block / marker server fn first, so server-only code never reaches JSX
    //    parsing/lowering. The JSX-aware extraction parses the marker pre-pass with JSX enabled so a
    //    top-level `$$`-suffixed (or `'use server'` / `'use websocket'`) server fn in a `.tjsx`/`.tsx`
    //    file is seen and lifted exactly like the `.ts` path — a plain-TS parse would choke on the
    //    component's JSX body and miss the marker, leaking the server body to the client.
    let extraction = extract_server_block_jsx(source);

    // 1b. Lower Angular control-flow blocks written directly inside JSX (`@if`/`@for`/`@switch`)
    //     out of the source *before* OXC parses it: their `{ … }` bodies are not parseable JSX
    //     expression containers (a multi-element / text / nested-block body makes the whole TSX
    //     parse fail). Each block is replaced by a `<treaty-cf-N />` placeholder element (valid JSX)
    //     and its lowered Angular HTML stashed; the placeholders are restored into the template
    //     after JSX lowering (step 3b). All later byte-offset work uses this preprocessed source.
    let preprocessed = angular_blocks::preprocess(&extraction.client_source);
    let client_source = preprocessed.source;
    let cf_blocks = preprocessed.blocks;

    // 2. Parse the client source as TSX (TypeScript + JSX).
    let allocator = Allocator::default();
    let source_type = SourceType::tsx();
    let ret = JsParser::new(&allocator, &client_source, source_type).parse();

    let mut errors: Vec<String> = ret.errors.iter().map(|e| e.to_string()).collect();

    // 3. Locate the component function and lower its returned JSX to a template HTML string. Seed
    //    the directive lowering pass with the imported class names first, so the bare-lowercase
    //    Angular-attribute directive form (`tooltip={x}` matching an imported `Tooltip`) resolves,
    //    and so every applied directive is collected for selectorless auto-import.
    let class_name = to_pascal_case(file_name);
    // Candidates come from the JSX-aware parse already in hand (`ret`): re-parsing the client source
    // as TS-only would choke on its JSX and yield no imports, so collect from `ret.program`. The
    // candidate set is the full IN-SCOPE symbol set — imported bindings AND top-level local
    // declarations — so a directive reference resolves to the REAL emitted identifier (`use:highlight`
    // → the local `highlight` function, `use:tooltip` → the imported `Tooltip`) rather than a
    // fabricated PascalCase name that is never defined.
    let mut directive_candidates = collect_import_names(&ret.program.body);
    directive_candidates.extend(collect_local_declaration_names(&ret.program.body));
    directives::begin_pass(&directive_candidates);
    let lowered = find_component(&ret.program.body, &client_source);
    let directive_refs = directives::take_directive_references();
    // A `use:`/capitalized/structural directive whose name matched no in-scope symbol resolves to
    // NOTHING: emitting `dependencies: [<Name>]` for it would dangle (the class is never imported or
    // declared), so the lowering dropped it from the dependency set and recorded it here. Surface a
    // diagnostic so the gap is explicit rather than a runtime `<Name> is not defined` at boot.
    let unresolved_directives = directives::take_unresolved_directives();

    // Props lowered to signal `input()`s carry their names (template auto-call) and any diagnostics
    // out of the located component so they survive the `match` that consumes it.
    let mut prop_signals: Vec<String> = Vec::new();
    let mut prop_diagnostics: Vec<String> = Vec::new();

    let (template_html, javascript) = match lowered {
        Some(component) => {
            prop_signals = component.prop_signals.clone();
            prop_diagnostics = component.prop_diagnostics.clone();
            // Assemble a FLAT module-top-level body: every top-level statement EXCEPT the component
            // declaration is kept verbatim (this hoists the author's `import`s and sibling helpers),
            // and the component declaration is replaced by its flattened inner body (the function /
            // arrow envelope and the JSX `return` are dropped). A bare `export default <Component>;`
            // statement naming the component is also dropped — the backend's `build_module` emits
            // its own single top-level `export default {Class};`. The result is the same flat shape
            // the `.treaty` path hands the shared backend, so no nested `export`/function envelope
            // can survive into the emitted module.
            let body = assemble_flat_body(
                &ret.program.body,
                &client_source,
                &component,
                &class_name,
            );
            (component.template_html, body)
        }
        None => {
            // F3: a `.tsx`/`.tjsx` may carry a base-Angular `@Component`-decorated CLASS rather than
            // the bare function/arrow JSX form. Route it to the `@Component` compiler (which lowers
            // `@Component` → ɵɵdefineComponent and is itself server-block aware), passing the ORIGINAL
            // source so that compiler runs its own server extraction + map redaction. This makes the
            // addon's documented "handles @Component JSX" claim hold instead of erroring with
            // "no component found". (`directives` pass state was already drained above, so the early
            // return leaks no thread-local state.)
            // Detect the decorator on the SERVER-STRIPPED `client_source` (the raw `source` still
            // carries the `server { … }` block, which is not valid bare TS and would fail the
            // detector's parse), but DELEGATE with the original `source` so `compile_angular_source`
            // runs its own server extraction + map redaction over the whole file.
            if crate::angular_source::has_angular_component(&client_source) {
                return crate::angular_source::compile_angular_source(source, file_name);
            }
            errors.push("jsx: no component (default-export or named function returning JSX) found".to_string());
            (String::new(), client_source.clone())
        }
    };

    // 3b. Restore the Angular control-flow blocks lifted in step 1b: swap each `<treaty-cf-N />`
    //     placeholder back to its lowered `@if`/`@for`/`@switch` HTML. Done before the signals pass
    //     so block-body interpolations are auto-called consistently with the rest of the template.
    let mut template_html = angular_blocks::restore(&template_html, &cf_blocks);

    // 4. When a server fn was lifted (a `server { … }` block, OR a top-level `$$` / `'use server'` /
    //    `'use websocket'` marker), emit it through the backend [`PluginRegistry`] default (axum +
    //    typesafe resource HTTP client) — the SAME default backend the `.ts`/`.treaty` paths use, not
    //    a hardcoded Elysia path — and rewrite the client call sites to the plugin's per-fn binding.
    //    The lifted server-fn body texts are kept so they can be redacted out of the client map's
    //    `sourcesContent` below. The emitted bindings reference the real `@treaty/httpclient` resource
    //    helper (`edenPromiseResource`); a real `import` of it is prepended so the client module
    //    resolves the binding at boot instead of throwing `<symbol> is not defined`.
    let (javascript, server_module, server_bodies, server_bindings) =
        if extraction.server_fns.is_empty() {
            (javascript, None, Vec::new(), std::collections::HashMap::new())
        } else {
            let registry = PluginRegistry::with_defaults();
            let plugin = registry
                .default_plugin()
                .expect("registry seeded with a default backend plugin");
            let emit = plugin.emit(&extraction.server_fns);
            let rewritten = rewrite_call_sites(&javascript, &emit.client_bindings);
            // Redact BOTH the stripped lifted `source` and the `verbatim_source` (the exact original
            // text, directive included) from the client map — the verbatim form is what the map's
            // `sourcesContent` embeds, so a `'use server'` marker fn (whose `source` had the directive
            // stripped) is still fully redacted.
            let mut bodies: Vec<String> =
                extraction.server_fns.iter().map(|f| f.source.clone()).collect();
            bodies.extend(extraction.server_fns.iter().map(|f| f.verbatim_source.clone()));
            (rewritten, Some(emit.server_module), bodies, emit.client_bindings)
        };

    // 4b. REACT-COMPAT PRE-NORMALIZATION: when the source is a plain React component (it imports from
    //     `react`/`react-dom`, or calls a React hook), lower its hook idioms to Angular primitives
    //     BEFORE the signals-by-default pass runs over the body — `useState`→`signal`, `setX`→`.set`/
    //     `.update`, `useEffect`→`effect`, `useMemo`→`computed`, `useCallback`→its fn, etc. (see
    //     [`react`]). Detection reuses the JSX-aware parse already in hand (`ret.program`); the
    //     rewrite is a no-op when no React idiom is present, so it is safe to run after the server
    //     rewrite over the final client JS. The signal names React produced are merged into the
    //     template auto-call set, and the `@angular/core` import for the emitted primitives is left to
    //     the signals pass (step 5), which owns the single merged import.
    let react_mode = react::detect_react_mode(&ret.program);
    let (javascript, mut react_signals, react_diagnostics) = if react_mode {
        let rt = react::transform(&javascript);
        // BODY AUTO-CALL (react mode only): a `useState`/`useMemo`/`useRef` signal or a props-as-
        // `input()` name read INSIDE the body (`count * 2`, `console.log(count)`, `prev + step`) is a
        // read of a signal *function* — it must be called to read the value, or a `computed` returns
        // `NaN` and an `effect` never re-runs. Run after the hook rewrites, over the union of the
        // React-discovered signals and the lowered prop names. This is SCOPED to react mode so the
        // plain `.tsx`/`.treaty` signals path (which auto-calls templates only, and which the
        // matchGolden corpus depends on) is left untouched.
        let mut body_call_signals = rt.signals.clone();
        body_call_signals.extend(prop_signals.iter().cloned());
        let javascript = react::auto_call_body(&rt.javascript, &body_call_signals);
        // TEMPLATE EVENT HANDLERS (react mode): an inline-arrow handler in the JSX was UNWRAPPED to
        // its body by the template lowering (so the template already carries `(click)="setCount(c =>
        // c + 1)"`), but its React setter call and any bare signal reads still need the same setter /
        // auto-call rewrites the body got. Run them now that the setters/signals are known:
        // `setCount(c => c + 1)` → `count.update(c => c + 1)`, a `setX(v)` → `x.set(v)`, a bare read →
        // a call. This is scoped to react mode for the same reason the body auto-call is.
        template_html =
            react::rewrite_template_handlers(&template_html, &rt.setters, &body_call_signals);
        (javascript, rt.signals, rt.diagnostics)
    } else {
        (javascript, std::collections::HashSet::new(), Vec::new())
    };

    // 5. Signals-by-default: every component variable is a signal. Wrap simple-value declarations in
    //    `signal(...)`, rewrite writes to `.set(...)` / `.update(...)`, and inject the `signal`
    //    import. The discovered signal names drive the template auto-call so a bare `{{ x }}` read of
    //    a signal becomes `{{ x() }}`. Run after the server + React rewrites so it sees the final
    //    client JS (React's emitted `signal(...)` are reactive primitives the signals pass skips).
    let transform = signals::transform(&javascript);
    let javascript = transform.javascript;
    // The auto-call candidate set is the union of the signals the pass wrapped, the props lowered to
    // `input()`s, and the names React lowered to `signal`/`computed`/`useRef`-signals — so a bare
    // `{{ x }}` read of ANY of them auto-calls to `{{ x() }}`.
    let mut auto_call_signals = transform.signals;
    auto_call_signals.extend(react_signals.drain());
    auto_call_signals.extend(prop_signals.iter().cloned());
    let template_html = signals::auto_call_template(&template_html, &auto_call_signals);

    // 6. Reuse the shared render3 backend. JSX components carry no `<style>` chunk yet, so styles
    //    are empty for this phase. The directive classes applied in the template (collected during
    //    lowering) are threaded in so they auto-import into the component's `dependencies`. The map
    //    embeds the ORIGINAL authoring source (`source`) as `sourcesContent`, named by `file_name`,
    //    exactly as the base `@Component` `.ts` and `.treaty` paths do.
    let (compiled, map) = compile_from_parts_with_directives_and_map(
        &class_name,
        &javascript,
        &template_html,
        "",
        file_name,
        &directive_refs,
        file_name,
        source,
    );

    // CLIENT PRIVACY: the map's `sourcesContent` is the original `.tjsx` source, which still carries
    // any `server { … }` block. Redact each lifted server-fn body out of the map content (blanked to
    // position-preserving whitespace) so the server source never reaches the client map — the same
    // guarantee the `.treaty` and base `@Component` paths provide.
    let map = map.map(|m| redact_server_bodies_in_map(&m, &server_bodies));

    let mut all_errors = errors;
    // Surface the non-fatal React-compat + props-lowering diagnostics (an un-lowerable `useReducer`,
    // a non-destructured `(props)` parameter, …). These are gaps the author should see, not hard
    // failures — the rest of the component still compiled.
    all_errors.extend(react_diagnostics);
    all_errors.extend(prop_diagnostics);
    // Surface every value-binding / structural `use:` directive that resolved to no in-scope symbol
    // (a genuinely dangling directive). The dependency was already dropped so the module does not
    // throw `<Name> is not defined` at boot, but the author still bound an input on / wrapped the
    // host with a directive that does not exist — report it so the gap is explicit at compile time.
    for name in &unresolved_directives {
        all_errors.push(format!(
            "jsx: directive `{name}` is applied with a binding but is not imported or declared in this module; \
             import the directive class so it resolves (the reference was dropped from `dependencies` to avoid a runtime `{name} is not defined`)"
        ));
    }
    all_errors.extend(compiled.errors);

    // The rewritten call sites in the emitted module reference the real `@treaty/httpclient` resource
    // helper the server-fn bindings wrap. Prepend a real MODULE-SCOPE `import` of it (a top-level
    // import statement, not inside the component wrapper where the signals pass would mistake it for
    // component state), so the module resolves the binding at boot rather than throwing `<symbol> is
    // not defined`. Empty when no binding symbol is referenced.
    // A lifted server fn that is a SIBLING module export (a top-level `$$` / `'use server'` /
    // `'use websocket'` fn declared beside the component, not invoked from the component body) was
    // removed by the lift, so an external `import { loadGreeting$$ } from './card'` would now receive
    // `undefined`. Re-export each such fn as its client binding at module scope so the consumer
    // transparently gets the RPC stub — the SAME wiring the `@Component` `.ts` and plain-`.ts` paths
    // apply, via the shared [`crate::plugin::export_server_fn_bindings`]. A fn already rewritten in the
    // component body (a free call) is skipped, so there is no double-binding.
    let with_bindings =
        crate::plugin::export_server_fn_bindings(&compiled.code, &extraction.server_fns, &server_bindings);

    let imports = client_runtime_imports_for_code(&with_bindings);
    let code = if imports.is_empty() {
        with_bindings
    } else {
        format!("{imports}\n{with_bindings}")
    };

    CompiledAuthoring {
        code,
        server_module,
        map,
        errors: all_errors,
    }
}

/// The raw lowering of a single component function/arrow, before module assembly: the lowered
/// template HTML and the component's FLAT inner body (statements inside the function/arrow body with
/// the JSX `return` removed). `find_component` wraps this with the enclosing declaration's span and
/// the component name to build a [`LoweredComponent`].
struct LoweredBody {
    template_html: String,
    /// The component function/arrow inner body, JSX `return` removed. Empty for an expression-bodied
    /// arrow (`() => <JSX/>`), which has no statements besides the returned JSX. When the component
    /// took a destructured props parameter, the synthesized `const <prop> = input(...)` declarations
    /// are PREPENDED here (and the param dropped), so each prop reads as a signal input.
    inner: String,
    /// Prop names lowered to signal inputs (for template auto-call).
    prop_signals: Vec<String>,
    /// Non-fatal props-lowering diagnostics.
    prop_diagnostics: Vec<String>,
}

/// Scan top-level statements for the component function and lower its returned JSX.
///
/// Recognized component forms:
///   * `export default function Name() { …; return <JSX/>; }`
///   * `export default () => <JSX/>` / `export default () => { …; return <JSX/>; }`
///   * a named `function Name() { …; return <JSX/>; }` declaration (with optional sibling
///     `export default Name;`)
///   * `const Name = () => <JSX/>` / `const Name = () => { …; return <JSX/>; }`
///   * `export function Name()` / `export const Name = () => …`
///
/// The first form that yields a JSX return wins. The returned [`LoweredComponent`] carries the whole
/// declaration statement's byte span (so it can be excised from the module-level body) plus the
/// component's flattened inner body.
fn find_component(body: &[Statement], source: &str) -> Option<LoweredComponent> {
    // Prefer the default export, then fall back to the first named function/arrow that returns JSX.
    for stmt in body {
        if let Statement::ExportDefaultDeclaration(export) = stmt {
            let (lowered, name) = match &export.declaration {
                ExportDefaultDeclarationKind::FunctionDeclaration(func) => (
                    lower_function(func, source),
                    func.id.as_ref().map(|id| id.name.to_string()),
                ),
                ExportDefaultDeclarationKind::ArrowFunctionExpression(arrow) => {
                    (lower_arrow(arrow, source), None)
                }
                _ => (None, None),
            };
            if let Some(lowered) = lowered {
                return Some(component_from(stmt, lowered, name));
            }
        }
    }

    for stmt in body {
        match stmt {
            Statement::FunctionDeclaration(func) => {
                if let Some(lowered) = lower_function(func, source) {
                    let name = func.id.as_ref().map(|id| id.name.to_string());
                    return Some(component_from(stmt, lowered, name));
                }
            }
            Statement::VariableDeclaration(decl) => {
                for declarator in &decl.declarations {
                    if let Some(Expression::ArrowFunctionExpression(arrow)) = &declarator.init {
                        if let Some(lowered) = lower_arrow(arrow, source) {
                            let name = declarator.id.get_identifier_name().map(|n| n.to_string());
                            return Some(component_from(stmt, lowered, name));
                        }
                    }
                }
            }
            // `export default` handled above; an `export function`/`export const` wraps the same
            // declaration kinds, so descend into it.
            Statement::ExportNamedDeclaration(export) => {
                if let Some(decl) = &export.declaration {
                    match decl {
                        Declaration::FunctionDeclaration(func) => {
                            if let Some(lowered) = lower_function(func, source) {
                                let name = func.id.as_ref().map(|id| id.name.to_string());
                                return Some(component_from(stmt, lowered, name));
                            }
                        }
                        Declaration::VariableDeclaration(var) => {
                            for declarator in &var.declarations {
                                if let Some(Expression::ArrowFunctionExpression(arrow)) =
                                    &declarator.init
                                {
                                    if let Some(lowered) = lower_arrow(arrow, source) {
                                        let name =
                                            declarator.id.get_identifier_name().map(|n| n.to_string());
                                        return Some(component_from(stmt, lowered, name));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Build a [`LoweredComponent`] from the located declaration statement, its raw lowering, and the
/// component's author-given name (if any). The declaration's whole byte span is recorded so the
/// module assembler can excise it and substitute the flattened inner body.
fn component_from(stmt: &Statement, lowered: LoweredBody, name: Option<String>) -> LoweredComponent {
    let span = oxc_span::GetSpan::span(stmt);
    LoweredComponent {
        template_html: lowered.template_html,
        declaration_span: (span.start as usize, span.end as usize),
        component_body: lowered.inner,
        name,
        prop_signals: lowered.prop_signals,
        prop_diagnostics: lowered.prop_diagnostics,
    }
}

/// Lower a `function` component: find the `return <JSX>` in its body, and extract the body's inner
/// statements with that return removed. A destructured props parameter is lowered to signal
/// `input()` declarations prepended to the inner body (and the param dropped — the JSX assembler
/// keeps only the inner body, so the parameter never survives).
fn lower_function(func: &Function, source: &str) -> Option<LoweredBody> {
    let body = func.body.as_deref()?;
    let return_span = jsx_return_span(&body.statements, source)?;
    let inner = body_inner_without_return(
        body.span.start as usize,
        body.span.end as usize,
        return_span,
        source,
    );
    let template_html = lower_jsx_return(&body.statements, source)?;
    let props = lower_props(&func.params, source);
    Some(props.into_body(template_html, inner))
}

/// Lower an arrow component: either an expression body that is JSX, or a block body with a
/// `return <JSX>`.
fn lower_arrow(arrow: &ArrowFunctionExpression, source: &str) -> Option<LoweredBody> {
    // An expression-bodied arrow (`() => <JSX/>`) stores the expression as a single `return`
    // statement in `body.statements` with `expression == true`. Its only content is the returned
    // JSX, so the flattened inner body is empty.
    if arrow.expression {
        let stmt = arrow.body.statements.first()?;
        if let Statement::ExpressionStatement(expr_stmt) = stmt {
            if let Some(html) = lower_jsx_expression(&expr_stmt.expression, source) {
                let props = lower_props(&arrow.params, source);
                return Some(props.into_body(html, String::new()));
            }
        }
        return None;
    }
    let return_span = jsx_return_span(&arrow.body.statements, source)?;
    let inner = body_inner_without_return(
        arrow.body.span.start as usize,
        arrow.body.span.end as usize,
        return_span,
        source,
    );
    let template_html = lower_jsx_return(&arrow.body.statements, source)?;
    let props = lower_props(&arrow.params, source);
    Some(props.into_body(template_html, inner))
}

/// The byte span of the first `return <JSX>` statement among `statements`, or `None`.
fn jsx_return_span(statements: &[Statement], source: &str) -> Option<(usize, usize)> {
    for stmt in statements {
        if let Statement::ReturnStatement(ret) = stmt {
            let argument = ret.argument.as_ref()?;
            if lower_jsx_expression(argument, source).is_some() {
                return Some((ret.span.start as usize, ret.span.end as usize));
            }
        }
    }
    None
}

/// Lower the first `return <JSX>` statement among `statements` to template HTML.
fn lower_jsx_return(statements: &[Statement], source: &str) -> Option<String> {
    for stmt in statements {
        if let Statement::ReturnStatement(ret) = stmt {
            let argument = ret.argument.as_ref()?;
            if let Some(html) = lower_jsx_expression(argument, source) {
                return Some(html);
            }
        }
    }
    None
}

/// Extract the interior of a function/arrow block body (`{ … }`, spanning `body_start..body_end`),
/// with the JSX `return` statement (`return_span`) removed. The enclosing braces are dropped so the
/// result is a flat statement list suitable for the shared backend's own component wrapper.
fn body_inner_without_return(
    body_start: usize,
    body_end: usize,
    return_span: (usize, usize),
    source: &str,
) -> String {
    // The body span includes the surrounding braces; the inner statements live strictly between
    // them. Guard the brace trim against a degenerate (empty) body.
    let inner_start = body_start.saturating_add(1).min(source.len());
    let inner_end = body_end.saturating_sub(1).max(inner_start);
    let (ret_start, ret_end) = return_span;

    let mut inner = String::with_capacity(inner_end - inner_start);
    // Everything from the body's first inner byte up to the JSX return.
    if ret_start > inner_start {
        inner.push_str(&source[inner_start..ret_start.min(inner_end)]);
    }
    // Everything after the JSX return up to the body's last inner byte. (Anything textually after a
    // `return` is dead code, but preserving it keeps the body byte-faithful for the rare early
    // helper-after-return; it is harmless inside the synthesized wrapper.)
    if ret_end < inner_end {
        inner.push_str(&source[ret_end..inner_end]);
    }
    inner
}

/// The result of lowering a component's props parameter: the synthesized signal-`input()`
/// declarations (a JS chunk to prepend to the component body), the prop names (for template
/// auto-call), and any non-fatal diagnostics.
struct LoweredProps {
    /// The `const <prop> = input(...);` declarations, newline-separated and terminated, or empty.
    decls: String,
    /// The lowered prop names (template auto-call candidates).
    names: Vec<String>,
    /// Non-fatal diagnostics (a non-destructured `(props)` parameter).
    diagnostics: Vec<String>,
}

impl LoweredProps {
    /// Build the final [`LoweredBody`], prepending the synthesized `input()` declarations to the
    /// component's inner body so each prop reads as a signal input that the backend extracts.
    fn into_body(self, template_html: String, inner: String) -> LoweredBody {
        let inner = if self.decls.is_empty() {
            inner
        } else if inner.trim().is_empty() {
            self.decls
        } else {
            format!("{}\n{inner}", self.decls)
        };
        LoweredBody {
            template_html,
            inner,
            prop_signals: self.names,
            prop_diagnostics: self.diagnostics,
        }
    }
}

/// Lower a component's PROPS parameter to Angular signal `input()` declarations.
///
/// The existing JSX front-end drops the component function's parameters entirely (only the inner
/// body survives module assembly), so a React/`.tsx` component that declared `props` had no inputs.
/// This closes that gap for BOTH React and our `.tsx`: a destructured object parameter
/// `function C({ a, b = 5 }: P)` lowers each property to a signal input —
///   * `a`      (no default) → `const a = input();`
///   * `b = 5`  (default)    → `const b = input(5);`
/// — and the prop names are recorded so a bare `{{ a }}` template read auto-calls. The signals pass
/// recognizes `input()` initializers as reactive primitives (it does not double-wrap them), and the
/// shared backend's `extract_io` turns the top-level `const a = input()` into the component's
/// `inputs`. A non-destructured `(props)` parameter cannot be enumerated into discrete inputs, so it
/// is left alone with a diagnostic (the "surface the gap, do not mis-compile" contract).
fn lower_props(params: &oxc_ast::ast::FormalParameters, source: &str) -> LoweredProps {
    use oxc_ast::ast::BindingPattern;

    let mut decls: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();

    // Only the FIRST parameter is the component's props (React + our `.tsx` convention). A second
    // parameter (e.g. a `ref`) is out of scope.
    if let Some(first) = params.items.first() {
        match &first.pattern {
            // `function C({ a, b = 5, label: caption })` — lower each destructured property to a
            // signal input. Shorthand (`a`), default (`b = 5`), AND rename (`label: caption`, where
            // the local is `caption` and the PUBLIC input name is `label`) are all handled.
            BindingPattern::ObjectPattern(obj) => {
                for prop in &obj.properties {
                    // The bound local name (for `{ a }` it is `a`; for `{ label: caption }` the local
                    // is `caption`).
                    let local = binding_pattern_local(&prop.value);
                    let Some(local) = local else {
                        diagnostics.push(
                            "jsx/props: a destructured prop is not a simple binding (nested \
                             destructure); skipped from `input()` lowering".to_string(),
                        );
                        continue;
                    };
                    // The PUBLIC input name is the property key; for `{ label: caption }` it is
                    // `label` (differs from the local `caption` — a rename). A computed key falls back
                    // to the local. When key == local (shorthand) there is no alias.
                    let public = property_key_text(&prop.key).unwrap_or_else(|| local.clone());
                    let alias = (public != local).then_some(public);
                    // A `b = 5` default lives on the property value's `AssignmentPattern` right side.
                    let default = binding_pattern_default(&prop.value, source).map(|d| {
                        // The default expression may carry TS (`'info' as AlertType`, `x satisfies T`,
                        // `f<T>()`) — it is spliced into runtime `input(<default>)`, so any TS syntax
                        // must be erased or the emitted `.mjs` is invalid JavaScript (the bare `as`
                        // breaks esbuild/Node). Erase via the shared AST pass; fall back to verbatim if
                        // the slice carries no TS (the common case re-parses to itself).
                        ts_erase::erase_via_reparse(&d).unwrap_or(d)
                    });
                    // A renamed prop carries its public name as the Angular `input(<default>, { alias:
                    // "<public>" })` option, so a parent still binds by the public name; the shared
                    // backend's `extract_io` reads this alias into the input's `binding_property_name`.
                    // Shorthand props keep the byte-identical single-arg `input()` / `input(<default>)`
                    // form the matchGolden corpus and the existing param tests depend on.
                    let arg = match (&default, &alias) {
                        (Some(d), Some(a)) => format!("{d}, {{ alias: {a:?} }}"),
                        (Some(d), None) => d.clone(),
                        (None, Some(a)) => format!("undefined, {{ alias: {a:?} }}"),
                        (None, None) => String::new(),
                    };
                    decls.push(format!("const {local} = input({arg});"));
                    names.push(local);
                }
                if obj.rest.is_some() {
                    diagnostics.push(
                        "jsx/props: a `...rest` props pattern cannot be enumerated into discrete \
                         inputs; the rest binding was dropped".to_string(),
                    );
                }
            }
            // A bare `(props)` identifier — the props are accessed as `props.x`, which cannot be
            // enumerated into discrete `input()`s at compile time. Surface a diagnostic; leave it.
            BindingPattern::BindingIdentifier(id) => {
                diagnostics.push(format!(
                    "jsx/props: a non-destructured props parameter `{}` cannot be lowered to discrete \
                     `input()`s; destructure the props (e.g. `{{ a, b }}`) so each becomes a signal input. \
                     (TODO: support member-access prop reads.)",
                    id.name
                ));
            }
            _ => {}
        }
    }

    LoweredProps {
        decls: if decls.is_empty() {
            String::new()
        } else {
            decls.join("\n")
        },
        names,
        diagnostics,
    }
}

/// The identifier text of an object-pattern property key (`label` for `{ label: caption }` /
/// `{ label }`), or `None` for a computed key.
fn property_key_text(key: &oxc_ast::ast::PropertyKey) -> Option<String> {
    use oxc_ast::ast::PropertyKey;
    match key {
        PropertyKey::StaticIdentifier(id) => Some(id.name.to_string()),
        PropertyKey::StringLiteral(s) => Some(s.value.to_string()),
        _ => None,
    }
}

/// The single local identifier bound by a (possibly defaulted) binding pattern: `a` for `a`, and the
/// `left` identifier for an `a = default` assignment pattern. `None` for a nested destructure.
fn binding_pattern_local(pat: &oxc_ast::ast::BindingPattern) -> Option<String> {
    use oxc_ast::ast::BindingPattern;
    match pat {
        BindingPattern::BindingIdentifier(id) => Some(id.name.to_string()),
        BindingPattern::AssignmentPattern(assign) => binding_pattern_local(&assign.left),
        _ => None,
    }
}

/// The verbatim default-value source text of a defaulted binding pattern (`5` for `b = 5`), or
/// `None` when the pattern has no default.
fn binding_pattern_default(pat: &oxc_ast::ast::BindingPattern, source: &str) -> Option<String> {
    use oxc_ast::ast::BindingPattern;
    let BindingPattern::AssignmentPattern(assign) = pat else {
        return None;
    };
    let span = oxc_span::GetSpan::span(&assign.right);
    Some(source[span.start as usize..span.end as usize].trim().to_string())
}

/// Lower an expression to template HTML if it is a JSX element or fragment.
fn lower_jsx_expression(expression: &Expression, source: &str) -> Option<String> {
    match expression {
        Expression::JSXElement(element) => Some(template::lower_element(element, source)),
        Expression::JSXFragment(fragment) => Some(template::lower_fragment(fragment, source)),
        // `return ( <JSX/> )` — OXC preserves the parentheses as a wrapper node; unwrap and recurse
        // so the common parenthesized-return authoring form is supported.
        Expression::ParenthesizedExpression(paren) => {
            lower_jsx_expression(&paren.expression, source)
        }
        _ => None,
    }
}

/// Assemble the FLAT module-top-level body the shared backend's `build_module` expects, from the
/// (preprocessed) `source`, the located `component`, and the derived `class_name`.
///
/// The shared backend re-wraps a flat component body inside its own `function {Class}() { … return
/// { bindings }; }` envelope and emits exactly one top-level `export default {Class};`. So the body
/// handed to it must NOT contain the author's enclosing `export default function`/`const X = () =>`
/// declaration (that was the malformed-module bug — a nested export + stray braces). This walks the
/// top-level statements and, for each:
///   * the located component declaration is REPLACED by its flattened inner body (the function/arrow
///     envelope and the JSX `return` already removed);
///   * a sibling bare `export default <Component>;` re-exporting the component by name is DROPPED
///     (the backend emits its own default export);
///   * a sibling NAMED `export` declaration (`export function highlight()`, `export const x = …`,
///     `export async function loadGreeting$$()`) has its `export ` keyword STRIPPED — only the inner
///     declaration is kept, so the helper becomes a plain component-body local rather than an illegal
///     nested `export` inside the synthesized wrapper. A re-export with no inner declaration
///     (`export { a }`, `export … from '…'`) carries nothing for the body and is dropped;
///   * every other statement (the author's `import`s, helper functions, type aliases, …) is kept
///     verbatim — `build_module` itself hoists any `import` declarations to module scope.
///
/// The result is the same flat shape the `.treaty` path produces, so it re-parses as a valid module
/// once wrapped, with no nested export and no stray braces.
fn assemble_flat_body(
    body: &[Statement],
    source: &str,
    component: &LoweredComponent,
    class_name: &str,
) -> String {
    let (decl_start, decl_end) = component.declaration_span;
    let mut out = String::with_capacity(source.len());

    for stmt in body {
        let span = oxc_span::GetSpan::span(stmt);
        let (start, end) = (span.start as usize, span.end as usize);

        // TS type-only declarations (`type X = …`, `interface X {}`) carry no runtime and are NOT
        // valid JavaScript — splicing them into the emitted `.mjs` makes Node/esbuild choke on the
        // bare `type`/`interface` keyword ("Expected ;"). Drop them.
        if matches!(
            stmt,
            Statement::TSTypeAliasDeclaration(_) | Statement::TSInterfaceDeclaration(_)
        ) {
            continue;
        }

        // The component declaration → its flattened inner body.
        if start == decl_start && end == decl_end {
            out.push_str(component.component_body.trim());
            out.push('\n');
            continue;
        }

        // A bare `export default <Component>;` (or `export default <Component>`) that re-exports the
        // component by name is dropped — the backend emits `export default {Class};` itself, and a
        // second top-level default export would be illegal. Match on the export's referenced name so
        // an unrelated `export default <expr>` is preserved (it would be a second component, out of
        // scope, and is harmless to keep).
        if is_default_export_of(stmt, component, class_name) {
            continue;
        }

        // A sibling NAMED `export` declaration (`export function highlight()`, `export const x = …`,
        // `export class Foo {}`, `export async function loadGreeting$$()`) is a helper the component
        // body references (a `use:` directive function, an inline server fn, a constant). Its WHOLE
        // statement — including the `export ` keyword — would otherwise be spliced into the body that
        // the shared backend re-wraps in `function {Class}() { … }`, producing an illegal nested
        // `export`. The backend hoists only `import`s to module scope, not these. So emit ONLY the
        // inner declaration (sliced from the declaration's own span, which excludes the leading
        // `export `): the helper becomes a plain component-body local — in scope for the component,
        // with no surviving `export` keyword. A re-export with no inner declaration (`export { a }`,
        // `export … from '…'`) carries nothing to keep in the body and is dropped.
        if let Statement::ExportNamedDeclaration(export) = stmt {
            if let Some(declaration) = &export.declaration {
                // `export type X = …` / `export interface X {}` are type-only — drop, don't splice
                // the inner type decl (it is not valid JS in the emitted module).
                use oxc_ast::ast::Declaration;
                if matches!(
                    declaration,
                    Declaration::TSTypeAliasDeclaration(_) | Declaration::TSInterfaceDeclaration(_)
                ) {
                    continue;
                }
                let decl_span = oxc_span::GetSpan::span(declaration);
                out.push_str(&source[decl_span.start as usize..decl_span.end as usize]);
                out.push('\n');
            }
            continue;
        }

        out.push_str(&source[start..end]);
        out.push('\n');
    }

    out
}

/// Whether `stmt` is a bare `export default <Ident>;` re-exporting the located component by its
/// author name (or by the derived `class_name`, in case the author already named it the class name).
fn is_default_export_of(
    stmt: &Statement,
    component: &LoweredComponent,
    class_name: &str,
) -> bool {
    let Statement::ExportDefaultDeclaration(export) = stmt else {
        return false;
    };
    let ExportDefaultDeclarationKind::Identifier(ident) = &export.declaration else {
        return false;
    };
    let referenced = ident.name.as_str();
    referenced == class_name
        || component.name.as_deref() == Some(referenced)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINE: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn pascal_case_from_tsx_name() {
        assert_eq!(to_pascal_case("hello-world.tsx"), "HelloWorld");
        assert_eq!(to_pascal_case("my_widget.tjsx"), "MyWidget");
        // A trailing `.component` segment is stripped, matching the `.treaty`/selector stem.
        assert_eq!(to_pascal_case("log-viewer.component.tsx"), "LogViewer");
        assert_eq!(to_pascal_case("src/foo/Bar.tsx"), "Bar");
    }

    #[test]
    fn class_name_and_selector_derive_from_file_name_not_body_symbols() {
        // Reproduces the reported defect: a lowercase default-export function (`counter`) alongside a
        // sibling helper/directive (`highlight`) must NOT name the emitted class after a body symbol.
        // The FILENAME is the source of truth → class `Counter`, selector `counter`.
        let source = "export function highlight() {}\n\
export default function counter() {\n  return <section>hi</section>;\n}\n";
        let out = compile(source, "counter.tsx");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("class Counter") || out.code.contains("function Counter("),
            "class must be filename-derived `Counter`, not a body symbol; got: {}",
            out.code
        );
        assert!(
            !out.code.contains("function highlight()") || out.code.contains("Counter"),
            "the sibling `highlight` must not become the component class; got: {}",
            out.code
        );
        // The selector is the filename-derived multi-form selector, not Angular's `ng-component`.
        assert!(
            out.code.contains("\"counter\"") && out.code.contains("\"Counter\""),
            "expected derived multi-form selector (counter/Counter); got: {}",
            out.code
        );
        assert!(
            !out.code.contains("ng-component"),
            "ng-component default must not survive; got: {}",
            out.code
        );
    }

    #[test]
    fn top_level_type_and_interface_declarations_are_stripped() {
        // Regression: a top-level TS `type X = …` / `interface Y {}` (and their `export` forms) are
        // type-only and must NOT survive into the emitted `.mjs` — they are not valid JavaScript, so
        // esbuild/Node choke on the bare `type`/`interface` keyword ("Expected ;"). The React Alert
        // showcase component declared `type AlertType = 'info' | …` and broke the packaged bundle.
        let source = "import { useState } from 'react';\n\
type AlertType = 'info' | 'warning' | 'error' | 'success';\n\
interface Props { kind: AlertType }\n\
export type Alias = string;\n\
export default function Alert({ kind }: Props) {\n  const [open, setOpen] = useState(true);\n  return <div className={kind}>{open}</div>;\n}\n";
        let out = compile(source, "alert.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(!code.contains("type AlertType"), "top-level `type` alias leaked: {code}");
        assert!(!code.contains("interface Props"), "top-level `interface` leaked: {code}");
        assert!(!code.contains("export type Alias"), "`export type` leaked: {code}");
        // The emitted client module must parse as valid JS/TS (no bare `type`/`interface` statement).
        assert_well_formed_module(code);
    }

    #[test]
    fn prop_default_with_a_type_cast_is_erased() {
        // Regression: a React/JSX prop default carrying TS (`type = 'info' as AlertType`) is spliced
        // into runtime `input(<default>)`; the `as`/generic/satisfies syntax must be erased or the
        // emitted `.mjs` is invalid JS (esbuild "Expected ;"). The Alert showcase component used
        // `'info' as AlertType` and failed to load. Must emit `input('info')`.
        let source = "type AlertType = 'info' | 'warn';\n\
export default function Alert({ kind = 'info' as AlertType, n = 0 }) {\n  return <div className={kind}>{n}</div>;\n}\n";
        let out = compile(source, "alert.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(!code.contains(" as AlertType"), "prop-default `as` cast leaked: {code}");
        assert!(code.contains("input('info')") || code.contains("input(\"info\")"), "default not erased to input('info'); got: {code}");
        assert_well_formed_module(code);
    }

    #[test]
    fn tjsx_multiword_file_name_derives_kebab_selector() {
        let source = "export default function greetingCard() {\n  return <section>hi</section>;\n}\n";
        let out = compile(source, "features/greeter/greeting-card.tjsx");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            out.code.contains("GreetingCard"),
            "class must be filename-derived `GreetingCard`; got: {}",
            out.code
        );
        assert!(
            out.code.contains("\"greeting-card\"")
                && out.code.contains("\"greetingCard\"")
                && out.code.contains("\"GreetingCard\""),
            "expected derived multi-form selector (greeting-card/greetingCard/GreetingCard); got: {}",
            out.code
        );
    }

    /// Parse `code` as an ES module and assert it is well-formed: no parse errors, exactly one
    /// top-level `export default`, and no `export`/`import` nested inside a function/block (the
    /// nested-export class of bug). Returns the parsed-OK result for the caller's further assertions.
    fn assert_well_formed_module(code: &str) {
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "emitted module did not RE-PARSE as a valid ES module: {:?}\n--- code ---\n{code}",
            parsed.errors
        );

        // Exactly one top-level `export default` — the backend's `export default {Class};`.
        let top_level_default_exports = parsed
            .program
            .body
            .iter()
            .filter(|s| matches!(s, Statement::ExportDefaultDeclaration(_)))
            .count();
        assert_eq!(
            top_level_default_exports, 1,
            "expected exactly one top-level `export default`, found {top_level_default_exports}\n--- code ---\n{code}"
        );

        // No `export`/`import` may appear anywhere other than the module top level. Re-parsing a body
        // that contains a nested `export default function …` succeeds only because OXC is lenient in
        // some configs; the robust guarantee is that the synthesized component wrapper
        // `function {Class}() { … }` contains NEITHER an `export` keyword NOR a nested function-scope
        // `export default`. Assert the wrapper body holds no `export ` token.
        if let Some(fn_idx) = code.find("function ") {
            // The component wrapper is the function whose body precedes the `.ɵfac` static assignment.
            if let Some(fac_idx) = code.find("\u{0275}fac") {
                let wrapper = &code[fn_idx..fac_idx];
                assert!(
                    !wrapper.contains("export "),
                    "an `export` leaked inside the synthesized component wrapper (nested export bug); got wrapper:\n{wrapper}"
                );
            }
        }
    }

    #[test]
    fn compiles_trivial_default_export_function_component() {
        let source = "export default function App() {\n  return <div>hi</div>;\n}\n";
        let out = compile(source, "app.tsx");

        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        assert!(out.code.contains("App"), "class name missing; got: {}", out.code);
        assert!(out.server_module.is_none(), "unexpected server module");
        // The lowered template text reached the backend.
        assert!(out.code.contains("App_Template"), "no template fn; got: {}", out.code);
    }

    #[test]
    fn compiles_arrow_expression_body_component() {
        let source = "const Widget = () => <span>x</span>;\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        assert!(out.code.contains("Widget"), "class name missing; got: {}", out.code);
    }

    #[test]
    fn compiles_parenthesized_return() {
        // The common `return ( <JSX/> )` form: OXC keeps the parens as a wrapper node.
        let source = "export default function App() {\n  return (\n    <div>hi</div>\n  );\n}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        assert!(out.code.contains("App_Template"), "no template fn; got: {}", out.code);
    }

    /// Parse `code` as an ES module (TypeScript) and assert it has no parse errors — by building the
    /// AST, never a regex — proving the emitted JSX client module is syntactically valid.
    fn assert_jsx_client_parses(code: &str) {
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let ret = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            ret.errors.is_empty(),
            "emitted JSX client did not parse: {:?}\n--- code ---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
    }

    /// Parse `code` and assert `name` is bound at MODULE SCOPE by a top-level `import` specifier local,
    /// read off the PARSED AST (never a regex).
    // Retained AST-based import checker (server-fn bindings are now imperative fetch and import no
    // resource helper, so it currently has no callers).
    #[allow(dead_code)]
    fn assert_jsx_imported_at_module_scope(code: &str, name: &str) {
        use oxc_ast::ast::ImportDeclarationSpecifier;
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let ret = JsParser::new(&allocator, code, module_type).parse();
        assert!(ret.errors.is_empty(), "client code did not parse: {code}");
        let imported = ret.program.body.iter().any(|stmt| {
            let Statement::ImportDeclaration(import) = stmt else { return false };
            let Some(specs) = &import.specifiers else { return false };
            specs.iter().any(|spec| {
                let local = match spec {
                    ImportDeclarationSpecifier::ImportSpecifier(s) => &s.local.name,
                    ImportDeclarationSpecifier::ImportDefaultSpecifier(s) => &s.local.name,
                    ImportDeclarationSpecifier::ImportNamespaceSpecifier(s) => &s.local.name,
                };
                local.as_str() == name
            })
        });
        assert!(imported, "`{name}` is not imported at module scope; got:\n{code}");
    }

    #[test]
    fn unified_jsx_with_dollar_suffix_fn_extracts_binds_and_imports() {
        // MATRIX (JSX + $$): a top-level `$$`-suffixed server fn in a `.tsx` module (whose component
        // body is JSX, so a plain-TS parse would choke and miss the marker) must be extracted via the
        // JSX-aware pre-pass, body ABSENT from the client, a client binding re-exported at module
        // scope, and the resource helper imported — the SAME unified wiring every front-end applies.
        let source = "import { signal } from '@angular/core'\n\
export async function loadGreeting$$(name: string) {\n\
  const greetings = ['Hello', 'Welcome'];\n\
  return { text: greetings[name.length] };\n\
}\n\
export default function greetingCard() {\n\
  const name = signal('Grace');\n\
  return <section>{name()}</section>;\n\
}\n";
        let out = compile(source, "greeting-card.tsx");

        let server_module = out
            .server_module
            .expect("JSX-file $$ fn must yield a server module");
        assert!(
            server_module.contains("greetings[name.length") || server_module.contains("'Hello'"),
            "body not carried into server module; got:\n{server_module}"
        );

        // VERIFY EMITTED CLIENT BY PARSING; body must be absent.
        assert_jsx_client_parses(&out.code);
        assert!(
            !out.code.contains("greetings[name.length"),
            "SECURITY: server body leaked into JSX client; got:\n{}",
            out.code
        );
        // The lifted fn is re-exported as its client binding at module scope.
        assert!(
            out.code.contains("export const loadGreeting$$ ="),
            "no re-exported client binding for the lifted $$ fn; got:\n{}",
            out.code
        );
        // The imperative binding does NOT wrap in `resource()`, so the resource helper is NOT imported.
        assert!(
            !out.code.contains("edenPromiseResource"),
            "imperative server-fn binding must not wrap in resource() (NG0203); got:\n{}",
            out.code
        );
    }

    #[test]
    fn in_component_server_block_emits_module_and_rewrites_call_site() {
        // A `.tsx` COMPONENT whose body declares an in-component `server { … }` block must yield a
        // non-None server_module and rewrite the client call site to the plugin's client binding —
        // same contract as a top-level block on the `.treaty` path.
        let source = "export default function App() {\n\
  function onSave(user) { return save(user); }\n\
  server {\n\
    async function save(user: User) { return db.insert(user); }\n\
  }\n\
  return <button onClick={onSave}>save</button>;\n\
}\n";
        let out = compile(source, "app.tsx");

        // The in-component server fn is extracted and emitted through the DEFAULT backend (axum +
        // typesafe resource HTTP client) — the same default the `.ts`/`.treaty` paths use.
        let server_module = out.server_module.expect("expected a server module for in-component block");
        assert!(
            server_module.contains("\"/__server/save\"") && server_module.contains("post(__server_save)"),
            "no save route in axum server module; got: {server_module}"
        );
        // The free call to `save` in the component body is rewritten to the axum client binding: an
        // imperative `fetch` POST to `/__server/save` (NOT a `resource()` wrapper, which threw NG0203
        // when called imperatively in an event handler outside an injection context), and the server
        // body never leaks into the client JS.
        assert!(
            out.code.contains("fetch('/__server/save'") && out.code.contains("'/__server/save'"),
            "call site not rewritten to the imperative axum fetch client; got: {}",
            out.code
        );
        // The imperative binding does NOT wrap in `resource()`, so the resource helper is NOT imported.
        assert!(
            !out.code.contains("edenPromiseResource"),
            "imperative server-fn binding must not wrap in resource() (NG0203); got: {}",
            out.code
        );
        assert!(
            !out.code.contains("db.insert"),
            "server body leaked into client JS; got: {}",
            out.code
        );
    }

    #[test]
    fn tjsx_without_server_block_carries_a_v3_map() {
        // A `.tjsx` component (no server block) compiles WITH an additive v3 map whose
        // `sourcesContent` embeds the original authoring source, named by the file.
        let source = "export default function App() {\n  return <div>hi</div>;\n}\n";
        let out = compile(source, "app.tjsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        let map = out.map.expect("expected a source map for a `.tjsx` component");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");
        assert_eq!(value["sources"][0], serde_json::json!("app.tjsx"), "wrong source name");
        let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
        assert!(
            contents.iter().any(|c| c.as_str() == Some(source)),
            "authoring source not embedded as sourcesContent; got: {map}"
        );
    }

    #[test]
    fn tjsx_server_block_body_is_absent_from_client_map() {
        // CLIENT PRIVACY: a `.tjsx` whose component body declares an in-component `server { … }`
        // block must compile to a v3 map whose `sourcesContent` does NOT contain the server body.
        let source = "export default function App() {\n\
  function onSave(user) { return save(user); }\n\
  server {\n\
    async function save(user: User) { return db.insert(user); }\n\
  }\n\
  return <button onClick={onSave}>save</button>;\n\
}\n";
        let out = compile(source, "app.tjsx");
        assert!(out.server_module.is_some(), "expected a server module");

        let map = out.map.expect("expected a source map for a server-block `.tjsx`");
        let value: serde_json::Value =
            serde_json::from_str(&map).expect("map should be valid JSON");
        assert_eq!(value["version"], serde_json::json!(3), "not a v3 map: {map}");

        let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
        for c in contents {
            let text = c.as_str().unwrap_or("");
            assert!(!text.contains("db.insert"), "server body leaked into map content: {text}");
            assert!(
                !text.contains("async function save"),
                "server signature leaked into map content: {text}"
            );
        }
        // The surviving client text (the handler) is still present in the redacted map.
        let joined: String = contents
            .iter()
            .filter_map(|c| c.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("function onSave"), "client body lost from map: {joined}");
    }

    #[test]
    fn tsx_component_class_routes_to_angular_compiler_without_leak() {
        // F3: a `@Component`-decorated CLASS in a `.tsx` is not the bare function/arrow JSX form, so
        // `find_component` yields None. Rather than erroring "no component found", the JSX front-end
        // now routes it to the base-Angular `@Component` compiler (the addon doc's "handles
        // @Component JSX" claim), which lowers it to `ɵɵdefineComponent`. The lifted server-fn body (a
        // stand-in secret) must be absent from BOTH the client code AND the map's sourcesContent — a
        // compile path that "just works" must never downgrade to a privacy leak.
        let source = "import { Component } from '@angular/core';\n\
server {\n\
  async function save(u: User) { return database.insert(u, SECRET_TOKEN); }\n\
}\n\
@Component({ template: '<button>x</button>' })\n\
export class Widget {}\n";
        let out = compile(source, "widget.tsx");

        // FUNCTIONAL: it actually compiles to an Ivy component now (delegated), not a "no component" error.
        assert!(
            out.code.contains("defineComponent") || out.code.contains("ɵɵdefineComponent"),
            "@Component-in-.tsx did not lower to ɵɵdefineComponent; got: {}",
            out.code
        );
        assert!(
            !out.errors.iter().any(|e| e.contains("no component")),
            "still erroring 'no component found' instead of delegating; errors: {:?}",
            out.errors
        );

        // SECURITY: the server body went to the backend, and the secret leaks into neither client nor map.
        assert!(out.server_module.is_some(), "server block should still be lifted to a backend");
        assert!(
            !out.code.contains("database.insert") && !out.code.contains("SECRET_TOKEN"),
            "server body leaked into client code; got: {}",
            out.code
        );
        if let Some(map) = out.map {
            let value: serde_json::Value = serde_json::from_str(&map).expect("map is valid JSON");
            let contents = value["sourcesContent"].as_array().expect("sourcesContent array");
            for c in contents {
                let text = c.as_str().unwrap_or("");
                assert!(
                    !text.contains("database.insert") && !text.contains("SECRET_TOKEN"),
                    "server body leaked into client map: {text}"
                );
            }
        }
    }

    #[test]
    fn compiles_tsx_with_non_ascii_without_panicking() {
        // FIX #1: a `.tsx` whose body and JSX template carry non-ASCII text (accented words + an
        // emoji) must compile without a mid-UTF-8-char byte-slice panic.
        let source = "export default function Saludo() {\n  \
const t\u{00ed}tulo = 'caf\u{00e9} \u{1F680}';\n  \
return <div title=\"na\u{00ef}ve \u{1F4A1}\">Hola caf\u{00e9} \u{1F600}</div>;\n}\n";
        let out = compile(source, "saludo.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
        // The non-ASCII template text survived lowering.
        assert!(
            out.code.contains("Hola caf\u{00e9}"),
            "non-ASCII template text lost; got: {}",
            out.code
        );
    }

    #[test]
    fn lowers_interpolation_container() {
        let source = "export default function App() {\n  const name = 'World';\n  return <div>{name}</div>;\n}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        // `{name}` lowers to Angular interpolation binding against the component context. `name` is a
        // signal-by-default (simple string init), so the read auto-calls: `ctx.name()`.
        assert!(out.code.contains("\u{0275}\u{0275}textInterpolate"), "no interpolation; got: {}", out.code);
        assert!(out.code.contains("ctx.name()"), "did not auto-call signal read; got: {}", out.code);
    }

    #[test]
    fn signals_by_default_end_to_end_counter() {
        // The headline acceptance case for signals-by-default: a `let count = 0` becomes a signal, a
        // template `{count}` read auto-calls (`{{ count() }}` → `ctx.count()`), and a `count++` in an
        // event handler lowers to `count.update(prev => prev + 1)`.
        let source = "export default function Counter() {\n\
  let count = 0;\n\
  const inc = () => { count++; };\n\
  return <button onClick={inc}>{count}</button>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // `count` is wrapped as a signal and the `signal` import is injected.
        assert!(
            code.contains("count = signal(0)"),
            "count not wrapped as signal; got: {code}"
        );
        assert!(
            code.contains("import { signal } from \"@angular/core\";"),
            "signal import missing; got: {code}"
        );
        // The template read auto-calls the signal: `{{ count() }}` binds `ctx.count()`.
        assert!(
            code.contains("ctx.count()"),
            "template did not auto-call signal read; got: {code}"
        );
        // The handler write lowers to the signal update API.
        assert!(
            code.contains("count.update(prev => prev + 1)"),
            "count++ not lowered to update; got: {code}"
        );
        // `inc` is behaviour, not state: it is NOT wrapped in `signal(...)`.
        assert!(
            !code.contains("signal(() =>"),
            "arrow handler wrongly wrapped as signal; got: {code}"
        );
    }

    #[test]
    fn directive_namespace_form_applies_and_auto_imports() {
        // PREFERRED form: `use:autofocus` applies the `Autofocus` directive and auto-imports it into
        // the component's dependencies via selectorless resolution (no manual imports array). The
        // directive class must be in scope (imported) so the dependency names a real symbol — the
        // selectorless contract drops the `imports: []` array, NOT the directive's import.
        let source = "import { Autofocus } from './autofocus';\n\
export default function Form() {\n  return <input use:autofocus />;\n}\n";
        let out = compile(source, "form.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The directive class lands in `dependencies` by selectorless auto-import.
        assert!(
            code.contains("dependencies: [Autofocus]")
                || code.contains("dependencies:[Autofocus]"),
            "Autofocus not auto-imported into dependencies; got: {code}"
        );
        // The host carries the directive's bare input attribute (no value-less directive is dropped).
        assert!(code.contains("autofocus"), "directive attr missing; got: {code}");
    }

    #[test]
    fn directive_namespace_form_binds_input_and_auto_imports() {
        // `use:tooltip={msg}` binds the directive's `tooltip` input and auto-imports `Tooltip`.
        let source = "import { Tooltip } from './tooltip';\n\
export default function Btn() {\n\
  const msg = 'hi';\n\
  return <button use:tooltip={msg}>x</button>;\n\
}\n";
        let out = compile(source, "btn.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            code.contains("dependencies: [Tooltip]") || code.contains("dependencies:[Tooltip]"),
            "Tooltip not auto-imported; got: {code}"
        );
        // The input binding reaches the template (a property instruction binds `tooltip`).
        assert!(
            code.contains("\u{0275}\u{0275}property") || code.contains("tooltip"),
            "tooltip input binding missing; got: {code}"
        );
    }

    #[test]
    fn directive_capitalized_attribute_applies_and_auto_imports() {
        // `<input Autofocus />` applies the `Autofocus` directive (the class is in scope by import).
        let source = "import { Autofocus } from './autofocus';\n\
export default function Form() {\n  return <input Autofocus />;\n}\n";
        let out = compile(source, "form.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(
            code.contains("dependencies: [Autofocus]")
                || code.contains("dependencies:[Autofocus]"),
            "Autofocus not auto-imported; got: {code}"
        );
    }

    #[test]
    fn directive_structural_form_lowers_and_auto_imports() {
        // A STRUCTURAL directive lowers to an `<ng-template>` host carrying the structural binding,
        // and the `Highlight` directive auto-imports. The JSX spelling of `*highlight` is the
        // `structural:highlight` namespace; the author imports `Highlight` so the name resolves.
        let source = "import { Highlight } from './highlight';\n\
export default function Card() {\n\
  const c = 'yellow';\n\
  return <div structural:highlight={c}>x</div>;\n\
}\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // Structural application: an embedded `<ng-template>` view is emitted (a nested template fn).
        assert!(
            code.contains("Card_Template") && code.contains("\u{0275}\u{0275}template"),
            "no embedded template for structural directive; got: {code}"
        );
        assert!(
            code.contains("dependencies: [Highlight]")
                || code.contains("dependencies:[Highlight]"),
            "Highlight not auto-imported; got: {code}"
        );
    }

    #[test]
    fn directive_bare_lowercase_matches_imported_class() {
        // The Angular-attribute form: a bare lowercase `tooltip` attribute matching an imported
        // `Tooltip` class applies the directive and auto-imports it.
        let source = "import { Tooltip } from './tooltip';\n\
export default function Btn() {\n\
  const msg = 'hi';\n\
  return <span tooltip={msg}>x</span>;\n\
}\n";
        let out = compile(source, "btn.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(
            code.contains("dependencies: [Tooltip]") || code.contains("dependencies:[Tooltip]"),
            "Tooltip not auto-imported; got: {code}"
        );
    }

    #[test]
    fn unused_directive_import_is_not_a_dependency() {
        // An imported directive that is never applied in the template must NOT become a dependency —
        // the "unused imports are not emitted" contract holds for directives too.
        let source = "import { Tooltip } from './tooltip';\n\
export default function Btn() {\n  return <span>x</span>;\n}\n";
        let out = compile(source, "btn.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(
            !out.code.contains("dependencies"),
            "unused directive import leaked into dependencies; got: {}",
            out.code
        );
    }

    /// Assert that `name` is referenced in the emitted `dependencies: […]` array AND is actually
    /// defined at MODULE scope in `code` (a top-level `import`/`function`/`class`/`const`, NOT buried
    /// inside the synthesized `function {Class}() { … }` wrapper where the module-scope
    /// `<Class>.ɵcmp = ɵɵdefineComponent({ dependencies: [name] })` could not see it). This is the
    /// guarantee that closes the dangling-`use:` bug: every `dependencies` entry resolves at boot.
    fn assert_dependency_defined_at_module_scope(code: &str, name: &str) {
        assert!(
            code.contains(&format!("dependencies: [{name}]"))
                || code.contains(&format!("dependencies:[{name}]"))
                || code.contains(&format!("[{name}]"))
                && code.contains("dependencies"),
            "`{name}` is not referenced in dependencies; got: {code}"
        );
        // The wrapper is `function {Class}() { … }`, ending just before the `.ɵfac` static. The
        // referenced symbol must be defined OUTSIDE that wrapper (module scope): either an import or a
        // top-level declaration appearing before the wrapper opens. Re-parse and check the top level.
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(
            parsed.errors.is_empty(),
            "emitted module did not re-parse: {:?}\n--- code ---\n{code}",
            parsed.errors
        );
        let defined_at_top = parsed.program.body.iter().any(|stmt| match stmt {
            Statement::ImportDeclaration(import) => import
                .specifiers
                .as_ref()
                .map(|specs| {
                    specs.iter().any(|s| {
                        use oxc_ast::ast::ImportDeclarationSpecifier::*;
                        match s {
                            ImportSpecifier(s) => s.local.name == name,
                            ImportDefaultSpecifier(s) => s.local.name == name,
                            ImportNamespaceSpecifier(s) => s.local.name == name,
                        }
                    })
                })
                .unwrap_or(false),
            Statement::FunctionDeclaration(func) => {
                func.id.as_ref().map(|id| id.name == name).unwrap_or(false)
            }
            Statement::ClassDeclaration(class) => {
                class.id.as_ref().map(|id| id.name == name).unwrap_or(false)
            }
            Statement::VariableDeclaration(var) => var
                .declarations
                .iter()
                .any(|d| d.id.get_identifier_name().as_deref() == Some(name)),
            _ => false,
        });
        assert!(
            defined_at_top,
            "dependency `{name}` is referenced but NOT defined at module scope (it would dangle / \
             `{name} is not defined` at boot); got: {code}"
        );
    }

    #[test]
    fn local_use_directive_is_defined_at_module_scope_and_referenced() {
        // The real `counter.tsx` shape: a sibling `export function highlight()` directive applied via
        // `use:highlight`. The `use:` name lowers to the class `Highlight`, but the author declared
        // the directive as the lowercase `highlight` — so the dependency must reference the REAL
        // identifier `highlight`, and that declaration must survive at MODULE scope (hoisted out of
        // the component wrapper) so `dependencies: [highlight]` resolves at boot rather than dangling.
        let source = "import { signal } from '@angular/core';\n\
export function highlight() {}\n\
export default function counter() {\n\
  const count = signal(0);\n\
  return <section use:highlight>{count()}</section>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        // The directive resolved to the author's real lowercase identifier — NOT a fabricated
        // `Highlight` — and it is defined at module scope (hoisted beside the component class).
        assert!(
            !code.contains("dependencies: [Highlight]") && !code.contains("dependencies:[Highlight]"),
            "fabricated PascalCase `Highlight` leaked into dependencies; got: {code}"
        );
        assert_dependency_defined_at_module_scope(code, "highlight");
        // The hoisted directive is NOT inside the component wrapper anymore: the wrapper's returned
        // bindings object must not list `highlight` (it is module-scope, not component state).
        let wrapper_start = code.find("function Counter() {").expect("no wrapper");
        let fac = code.find("\u{0275}fac").expect("no fac");
        let wrapper = &code[wrapper_start..fac];
        assert!(
            !wrapper.contains("function highlight"),
            "directive left inside the component wrapper (would dangle at module scope); got: {wrapper}"
        );
    }

    #[test]
    fn imported_use_directive_is_hoisted_and_referenced() {
        // An IMPORTED `use:` directive: `use:tooltip={msg}` matches the imported `Tooltip`. The import
        // must be hoisted to module scope (above the wrapper) and `dependencies: [Tooltip]` references
        // it — the import is the in-scope symbol, so the dependency resolves at boot.
        let source = "import { Tooltip } from './tooltip';\n\
export default function Btn() {\n\
  const msg = 'hi';\n\
  return <button use:tooltip={msg}>x</button>;\n\
}\n";
        let out = compile(source, "btn.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert_dependency_defined_at_module_scope(code, "Tooltip");
        // The import sits above the component wrapper (module scope), not inside it.
        let import_idx = code
            .find("import { Tooltip } from './tooltip';")
            .expect("tooltip import missing");
        let wrapper_idx = code.find("function Btn() {").expect("no wrapper");
        assert!(import_idx < wrapper_idx, "import not hoisted above wrapper; got: {code}");
    }

    #[test]
    fn value_less_unresolved_use_directive_drops_dependency_without_error() {
        // The real `greeting-card.tjsx` shape: `use:autofocus` with NO `autofocus`/`Autofocus` symbol
        // in scope. It must NOT emit a dangling `dependencies: [Autofocus]` (which would throw
        // `Autofocus is not defined` at boot); instead it degrades to a bare native `autofocus` host
        // attribute, contributing no dependency and no diagnostic.
        let source = "import { signal } from '@angular/core';\n\
export default function greetingCard() {\n\
  const name = signal('Grace');\n\
  return <input use:autofocus value={name()} />;\n\
}\n";
        let out = compile(source, "greeting-card.tjsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(
            !code.contains("Autofocus"),
            "fabricated/dangling `Autofocus` leaked into the emit; got: {code}"
        );
        // The native `autofocus` attribute still reached the template (the directive degraded to it).
        assert!(code.contains("autofocus"), "native autofocus attribute lost; got: {code}");
    }

    #[test]
    fn value_binding_unresolved_use_directive_is_reported() {
        // A value-BINDING `use:tooltip={x}` with no `Tooltip` in scope binds an `@Input` that cannot
        // exist — a genuine dangling-directive mistake. The dependency is dropped (no boot-time
        // `Tooltip is not defined`) AND a diagnostic is surfaced so the gap is explicit.
        let source = "export default function Btn() {\n\
  const msg = 'hi';\n\
  return <button use:tooltip={msg}>x</button>;\n\
}\n";
        let out = compile(source, "btn.tsx");
        assert!(
            out.errors.iter().any(|e| e.contains("Tooltip") && e.contains("not imported")),
            "expected an unresolved-directive diagnostic; got: {:?}",
            out.errors
        );
        assert!(
            !out.code.contains("dependencies: [Tooltip]")
                && !out.code.contains("dependencies:[Tooltip]"),
            "dangling Tooltip dependency emitted; got: {}",
            out.code
        );
    }

    #[test]
    fn realistic_counter_tsx_compiles_all_features_to_ivy() {
        // Headline end-to-end acceptance: a realistic Counter component that exercises, in one source,
        // every JSX authoring feature and must compile all the way to Ivy `defineComponent` output:
        //   * signals-by-default state (`let count = 0` / `let items = [...]`),
        //   * an `onClick` event handler that increments (`count++`),
        //   * a static `class` attribute,
        //   * a `{count}` text interpolation that auto-calls the signal,
        //   * an `items.map(...)` lowered to an `@for` list,
        //   * a `{cond && <JSX/>}` lowered to an `@if`,
        //   * a `use:` directive (selectorless auto-import; the class is in scope by import).
        let source = "import { Autofocus } from './autofocus';\n\
export default function Counter() {\n\
  let count = 0;\n\
  let items = [1, 2, 3];\n\
  const inc = () => { count++; };\n\
  return <div class=\"counter\">\n\
      <button onClick={inc} use:autofocus>{count}</button>\n\
      {count() > 0 && <p>positive</p>}\n\
      <ul>\n\
        {items.map((item) => <li>{item}</li>)}\n\
      </ul>\n\
    </div>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        // (0) Compiled all the way to an Ivy component definition.
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("Counter"), "class name missing; got: {code}");
        assert!(code.contains("Counter_Template"), "no template fn; got: {code}");

        // (1) signals-by-default: `count` and `items` are wrapped, `signal` is imported, `inc` is not
        //     state so it is never wrapped.
        assert!(code.contains("count = signal(0)"), "count not a signal; got: {code}");
        assert!(code.contains("items = signal("), "items not a signal; got: {code}");
        assert!(
            code.contains("import { signal } from \"@angular/core\";"),
            "signal import missing; got: {code}"
        );
        assert!(!code.contains("signal(() =>"), "handler wrongly wrapped; got: {code}");

        // (2) onClick increment lowers to the signal update API and binds a listener instruction.
        assert!(
            code.contains("count.update(prev => prev + 1)"),
            "count++ not lowered to update; got: {code}"
        );
        assert!(
            code.contains("\u{0275}\u{0275}listener"),
            "no listener instruction for onClick; got: {code}"
        );

        // (3) the static `class` reaches the const pool / element instruction.
        assert!(code.contains("counter"), "class attr missing; got: {code}");

        // (4) `{count}` interpolation auto-calls the signal read.
        assert!(
            code.contains("\u{0275}\u{0275}textInterpolate"),
            "no interpolation instruction; got: {code}"
        );
        assert!(code.contains("ctx.count()"), "signal read not auto-called; got: {code}");

        // (5) `items.map(...)` lowered to an `@for` → Ivy repeater.
        assert!(
            code.contains("\u{0275}\u{0275}repeaterCreate") || code.contains("\u{0275}\u{0275}repeater"),
            "items.map did not lower to a repeater; got: {code}"
        );

        // (6) `{cond && <p/>}` lowered to an `@if` → Ivy conditional / embedded template.
        assert!(
            code.contains("\u{0275}\u{0275}conditional") || code.contains("\u{0275}\u{0275}template"),
            "&& did not lower to a conditional; got: {code}"
        );

        // (7) the `use:autofocus` directive auto-imports selectorlessly.
        assert!(
            code.contains("dependencies: [Autofocus]")
                || code.contains("dependencies:[Autofocus]"),
            "Autofocus directive not auto-imported; got: {code}"
        );
    }

    #[test]
    fn angular_if_block_in_jsx_lowers_and_compiles_to_ivy() {
        // An Angular `@if` block written DIRECTLY inside the returned JSX (not the `&&`/ternary
        // idiom) lowers to an Angular `@if` and compiles to Ivy. OXC cannot parse the block's `{ … }`
        // body as a JSX expression container, so it is lifted out of the source before parsing and
        // restored into the template after lowering.
        let source = "export default function App() {\n\
  const show = true;\n\
  return <div>@if (show) { <p>hi</p> }</div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The block lowered to an Ivy conditional / embedded template (the `@if` reached render3).
        assert!(
            code.contains("\u{0275}\u{0275}conditional") || code.contains("\u{0275}\u{0275}template"),
            "@if did not lower to a conditional; got: {code}"
        );
        // The condition reached the Ivy conditional binding (`ctx.show ? 1 : -1`).
        assert!(
            code.contains("ctx.show"),
            "@if condition lost; got: {code}"
        );
    }

    #[test]
    fn angular_for_block_in_jsx_lowers_and_compiles_to_ivy() {
        // An Angular `@for` block written directly inside JSX lowers to `@for` → Ivy repeater.
        let source = "export default function List() {\n\
  let xs = [1, 2, 3];\n\
  return <ul>@for (x of xs; track x) { <li>{x}</li> }</ul>;\n\
}\n";
        let out = compile(source, "list.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // `@for` lowered to an Ivy repeater.
        assert!(
            code.contains("\u{0275}\u{0275}repeaterCreate") || code.contains("\u{0275}\u{0275}repeater"),
            "@for did not lower to a repeater; got: {code}"
        );
    }

    #[test]
    fn angular_if_else_block_in_jsx_compiles_to_ivy() {
        // `@if (…) { … } @else { … }` written directly in JSX compiles, exercising the continuation
        // (`@else`) parsing of the block scanner.
        let source = "export default function App() {\n\
  const ok = true;\n\
  return <div>@if (ok) { <p>yes</p> } @else { <p>no</p> }</div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}conditional") || code.contains("\u{0275}\u{0275}template"),
            "@if/@else did not lower to a conditional; got: {code}"
        );
        // The `@else` produced a two-arm conditional: the selector picks branch 1 or 2 on the
        // condition, not the single-arm `? 1 : -1` form. `ok` is a signal-by-default (`const ok =
        // true`), so the `@if` head auto-calls the signal read: `ctx.ok() ? 1 : 2` (GAP 1).
        assert!(
            code.contains("ctx.ok() ? 1 : 2"),
            "@else branch not emitted as a second auto-called conditional arm; got: {code}"
        );
    }

    #[test]
    fn angular_for_block_body_signal_read_auto_calls_in_template() {
        // The restored control-flow block is part of the template HTML *before* the signals pass, so
        // a `{count}` read inside a `@for` body auto-calls the signal exactly like ordinary template
        // text. This is asserted at the template layer (the JSX front-end owns it); the render3
        // backend emits the auto-called read into the block's embedded view function.
        use std::collections::HashSet;
        let source = "export default function App() {\n\
  let count = 0;\n\
  let xs = [1];\n\
  return <ul>@for (x of xs; track x) { <li>{count}</li> }</ul>;\n\
}\n";
        // Lower the source the way `compile` does up to the template, then run the signals auto-call.
        let extraction = crate::plugin::extract_server_block(source);
        let pre = angular_blocks::preprocess(&extraction.client_source);
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, &pre.source, SourceType::tsx()).parse();
        let lowered = find_component(&ret.program.body, &pre.source).expect("component");
        let template = angular_blocks::restore(&lowered.template_html, &pre.blocks);
        let signals: HashSet<String> = ["count".to_string(), "xs".to_string()].into_iter().collect();
        let template = signals::auto_call_template(&template, &signals);
        assert!(
            template.contains("count()"),
            "block-body signal read not auto-called in template; got: {template}"
        );
        // And the whole thing still compiles to Ivy end to end.
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn angular_switch_block_in_jsx_compiles_to_ivy() {
        // A `@switch`/`@case`/`@default` block — the form OXC outright rejects (nested `@case` chain
        // inside the switch braces) — compiles end to end after the block preprocessor lifts it out.
        let source = "export default function App() {\n\
  let v = 1;\n\
  return <div>@switch (v) { @case (1) { <p>one</p> } @default { <p>other</p> } }</div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn angular_block_and_jsx_idioms_coexist() {
        // A direct `@if` block alongside the `.map` JSX idiom in the same component: the preprocessor
        // lifts only the `@`-block, leaving the `.map` for the ordinary JSX control-flow lowering.
        let source = "export default function App() {\n\
  let show = true;\n\
  let xs = [1, 2];\n\
  return <div>@if (show) { <p>hi</p> }<ul>{xs.map(x => <li>{x}</li>)}</ul></div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The `@if` lowered to a conditional and the `.map` lowered to a repeater.
        assert!(code.contains("ctx.show"), "@if condition lost; got: {code}");
        assert!(
            code.contains("\u{0275}\u{0275}repeaterCreate") || code.contains("\u{0275}\u{0275}repeater"),
            ".map did not lower to a repeater; got: {code}"
        );
    }

    #[test]
    fn signals_by_default_skips_reactive_primitives_end_to_end() {
        // An existing `input()` / `computed(...)` must not be double-wrapped, and every referenced
        // reactive primitive is pulled into ONE merged `@angular/core` import (so the emitted module
        // resolves `signal`, `computed`, AND the author's `input` at boot — `input` was previously
        // left unimported, a latent `input is not defined`).
        let source = "export default function Widget() {\n\
  const label = input('hi');\n\
  let count = 0;\n\
  const doubled = computed(() => count() * 2);\n\
  return <div>{doubled} {label}</div>;\n\
}\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert!(
            !code.contains("signal(input(") && !code.contains("signal(computed("),
            "reactive primitive double-wrapped; got: {code}"
        );
        assert!(
            code.contains("count = signal(0)"),
            "plain count not wrapped; got: {code}"
        );
        // Exactly one merged `@angular/core` import binds `signal`, `computed`, and `input` (verified
        // off the parsed AST, never a substring race).
        for name in ["signal", "computed", "input"] {
            assert!(
                core_import_binds(code, name),
                "`{name}` not imported from a single merged @angular/core import; got: {code}"
            );
        }
    }

    /// Whether `code` binds `name` via a named specifier of an `@angular/core` import, read off the
    /// parsed module AST (never a substring scan).
    fn core_import_binds(code: &str, name: &str) -> bool {
        use oxc_ast::ast::ImportDeclarationSpecifier;
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let ret = JsParser::new(&allocator, code, module_type).parse();
        assert!(ret.errors.is_empty(), "emitted module did not parse: {code}");
        ret.program.body.iter().any(|stmt| {
            let Statement::ImportDeclaration(import) = stmt else { return false };
            if import.source.value.as_str() != "@angular/core" {
                return false;
            }
            import
                .specifiers
                .as_ref()
                .map(|specs| {
                    specs.iter().any(|s| {
                        matches!(s, ImportDeclarationSpecifier::ImportSpecifier(s) if s.local.name == name)
                    })
                })
                .unwrap_or(false)
        })
    }

    // --- Module assembly: the emitted module must be a VALID re-parseable ES module ------------
    // These guard the malformed-module bug: the JSX front-end used to splice the author's
    // `export default function …` verbatim inside a synthesized `function {Class}() { … }` wrapper,
    // producing an illegal nested export, stray braces, and an empty `return {}`. The fix flattens
    // the component body and lets the backend emit exactly one top-level `export default {Class};`.

    #[test]
    fn reported_bug_about_reparses_as_valid_module() {
        // The exact reported case: `export default function About(){ const team=[1,2,3]; return
        // <div>{team.length}</div> }`. It must emit a VALID module — no nested `export default`, no
        // stray brace, no empty `return {}` — and signals-by-default still applies.
        let source = "export default function About() {\n\
  const team = [1, 2, 3];\n\
  return <div>{team.length}</div>;\n\
}\n";
        let out = compile(source, "About.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The synthesized wrapper exists and the backend's single default export is the class.
        assert!(code.contains("function About() {"), "no component wrapper; got: {code}");
        assert!(code.contains("export default About;"), "no class default export; got: {code}");
        // The author's `export default function About` does NOT survive as a nested declaration.
        assert!(
            !code.contains("export default function About"),
            "author default-export function leaked into the body; got: {code}"
        );
        // signals-by-default lowered the array initializer.
        assert!(code.contains("team = signal([1, 2, 3])"), "team not a signal; got: {code}");
    }

    #[test]
    fn named_function_with_separate_default_export_reparses() {
        // `function Card(){ … } export default Card;` — the named function is the component and the
        // sibling `export default Card;` must be DROPPED (the backend emits its own default export),
        // leaving exactly one top-level default export.
        let source = "function Card() {\n\
  const label = 'hi';\n\
  return <span>{label}</span>;\n\
}\n\
export default Card;\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("export default Card;"), "no class default export; got: {code}");
        // The author's named `function Card` does not survive as a nested declaration in the wrapper.
        assert!(
            !code.contains("return <span>"),
            "JSX return leaked into body; got: {code}"
        );
    }

    #[test]
    fn const_arrow_with_separate_default_export_reparses() {
        // `const Widget = () => { … }; export default Widget;` — same contract for the arrow form.
        let source = "const Widget = () => {\n\
  const n = 5;\n\
  return <p>{n}</p>;\n\
};\n\
export default Widget;\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(code.contains("export default Widget;"), "no class default export; got: {code}");
        // signals-by-default applied to the inner declaration.
        assert!(code.contains("n = signal(5)"), "n not a signal; got: {code}");
    }

    #[test]
    fn arrow_expression_body_reparses_as_valid_module() {
        // `const Widget = () => <span>x</span>;` (no block body): the flattened inner body is empty,
        // and the emitted module is still valid with one default export.
        let source = "const Widget = () => <span>x</span>;\n";
        let out = compile(source, "widget.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert_well_formed_module(&out.code);
        assert!(out.code.contains("export default Widget;"), "no default export; got: {}", out.code);
    }

    #[test]
    fn import_plus_default_export_function_reparses() {
        // The author's top-level `import` must be HOISTED to module scope (never spliced into the
        // wrapper), and the `export default function App` must NOT nest. This is the App.tjsx form.
        let source = "import { Helper } from './helper';\n\
export default function App() {\n\
  const greeting = 'hi';\n\
  return <div>{greeting}</div>;\n\
}\n";
        let out = compile(source, "App.tjsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        // The user import is hoisted above the wrapper (module scope), not inside it.
        let import_idx = code
            .find("import { Helper } from './helper';")
            .expect("user import missing");
        let wrapper_idx = code.find("function App() {").expect("no wrapper");
        assert!(
            import_idx < wrapper_idx,
            "user import not hoisted above the wrapper; got: {code}"
        );
        assert!(
            !code.contains("export default function App"),
            "author default-export function leaked into the body; got: {code}"
        );
    }

    #[test]
    fn sibling_export_helper_is_stripped_not_nested() {
        // The real `counter.tsx` shape: a sibling `export function highlight()` helper alongside the
        // default-export component. Its `export ` keyword must be STRIPPED (kept as a plain body
        // local), never spliced verbatim into the synthesized `function {Class}() { … }` wrapper —
        // a nested `export` is illegal and was the second module-assembly bug.
        let source = "import { signal } from '@angular/core';\n\
export function highlight() {}\n\
export default function counter() {\n\
  const count = signal(0);\n\
  return <button use:highlight>{count()}</button>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        // The helper survives as a plain (non-export) declaration inside the wrapper.
        assert!(code.contains("function highlight() {}"), "helper lost; got: {code}");
        // No sibling `export function highlight` survives anywhere — it was de-exported.
        assert!(
            !code.contains("export function highlight"),
            "sibling export not stripped (nested export bug); got: {code}"
        );
        assert!(code.contains("export default Counter;"), "no class default export; got: {code}");
    }

    #[test]
    fn sibling_dollar_marked_export_fn_is_extracted_as_server_fn() {
        // The real `greeting-card.tjsx` shape: a sibling `export async function loadGreeting$$()`
        // alongside the default-export component, referenced from the component body. The `$$` suffix
        // is the inline server-fn marker, so this helper is SERVER-ONLY: the JSX front-end must
        // extract its body to a server module (axum default) and rewrite the component's call site to
        // the typed client binding — the body must NOT survive as a client-side body local (that was
        // the security leak this closes).
        let source = "import { signal } from '@angular/core';\n\
export async function loadGreeting$$(name) {\n\
  const greetings = ['Hello', 'Welcome'];\n\
  return { text: greetings[name.length % greetings.length] };\n\
}\n\
export default function greetingCard() {\n\
  const name = signal('Grace');\n\
  const greet = async () => { await loadGreeting$$(name()); };\n\
  return <button onClick={greet}>{name()}</button>;\n\
}\n";
        let out = compile(source, "greeting-card.tjsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        // A server module carries the lifted body (axum default backend).
        let server_module = out
            .server_module
            .expect("the `$$`-marked sibling fn must be lifted to a server module");
        assert!(
            server_module.contains("\"/__server/loadGreeting$$\"")
                || server_module.contains("__server_loadGreeting"),
            "no server route for the lifted `$$` fn; got: {server_module}"
        );
        // SECURITY: the server-fn body (the `greetings` data + indexing) is ABSENT from the client.
        assert!(
            !code.contains("['Hello', 'Welcome']") && !code.contains("greetings[name.length"),
            "SECURITY: `$$` server body leaked into the client; got: {code}"
        );
        // The declaration is gone (neither exported nor a surviving body declaration with the body).
        assert!(
            !code.contains("async function loadGreeting$$")
                && !code.contains("export async function loadGreeting$$"),
            "the `$$` server fn declaration leaked into the client; got: {code}"
        );
        // The call site routes through the axum client binding: an imperative `fetch` POST to the
        // server route (NOT a `resource()` wrapper, which threw NG0203 when a server fn was called
        // imperatively in an event handler outside an injection context), so the resource helper is
        // NOT imported.
        assert!(
            code.contains("fetch('/__server/loadGreeting$$'"),
            "call site not rewritten to the imperative axum fetch binding; got: {code}"
        );
        assert!(
            !code.contains("edenPromiseResource"),
            "imperative server-fn binding must not wrap in resource() (NG0203); got: {code}"
        );
        assert!(code.contains("export default GreetingCard;"), "no class default export; got: {code}");
    }

    /// Collect every top-level (and `export`-wrapped) function/arrow-const NAME declared in `code` by
    /// walking the PARSED tsx AST (not a regex). Used to assert a lifted `$$` server fn's DECLARATION
    /// is absent from the emitted client module — a surviving declaration is a real AST node.
    fn jsx_client_declared_fn_names(code: &str) -> Vec<String> {
        let allocator = Allocator::default();
        let ret = JsParser::new(&allocator, code, SourceType::tsx()).parse();
        assert!(
            ret.errors.is_empty(),
            "emitted client module did not parse as tsx: {:?}\n--- code ---\n{code}",
            ret.errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        );
        let mut names = Vec::new();
        for stmt in &ret.program.body {
            match stmt {
                Statement::FunctionDeclaration(f) => {
                    if let Some(id) = &f.id {
                        names.push(id.name.to_string());
                    }
                }
                Statement::VariableDeclaration(d) => {
                    for decl in &d.declarations {
                        if let (Some(name), Some(Expression::ArrowFunctionExpression(_))) =
                            (decl.id.get_identifier_name(), &decl.init)
                        {
                            names.push(name.to_string());
                        }
                    }
                }
                _ => {}
            }
        }
        names
    }

    #[test]
    fn jsx_dollar_marked_server_fn_body_absent_from_parsed_client() {
        // PHASE 1: a `$$`-marked server fn in a JSX (`.tjsx`) file must be extracted exactly like the
        // `.ts` path. Verified by PARSING the emitted client (tsx): the body statements are ABSENT,
        // the fn is not a surviving declaration, a binding is present, and a server module is produced.
        let source = "import { signal } from '@angular/core';\n\
export async function loadGreeting$$(name: string) {\n\
  const greetings = ['Hello', 'Welcome', 'Greetings', 'Salutations'];\n\
  return { text: greetings[name.length % greetings.length] };\n\
}\n\
export default function greetingCard() {\n\
  const name = signal('Grace');\n\
  const greet = async () => { await loadGreeting$$(name()); };\n\
  return <button onClick={greet}>{name()}</button>;\n\
}\n";
        let out = compile(source, "greeting-card.tjsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        // serverModule is populated (the lifted body lives there, not on the client).
        let server_module = out.server_module.expect("expected a server module for the `$$` JSX fn");
        assert!(
            server_module.contains("greetings") || server_module.contains("__server_loadGreeting"),
            "server module did not carry the lifted fn; got:\n{server_module}"
        );

        // PARSE the client: the body data is absent and the fn is not a surviving declaration.
        let names = jsx_client_declared_fn_names(&out.code);
        assert!(
            !names.contains(&"loadGreeting$$".to_string()),
            "the `$$` server fn survived as a client declaration; got names: {names:?}"
        );
        assert!(
            !out.code.contains("['Hello', 'Welcome', 'Greetings', 'Salutations']")
                && !out.code.contains("greetings[name.length"),
            "SECURITY: `$$` server body leaked into the client; got:\n{}",
            out.code
        );
        // A binding is present: an imperative `fetch` POST to the server route, NOT a `resource()`
        // wrapper (which threw NG0203 when a server fn was called imperatively in an event handler
        // outside an injection context), so the resource helper is NOT imported.
        assert!(
            out.code.contains("fetch('/__server/loadGreeting$$'"),
            "no imperative fetch binding for the lifted fn; got:\n{}",
            out.code
        );
        assert!(
            !out.code.contains("edenPromiseResource"),
            "imperative server-fn binding must not wrap in resource() (NG0203); got:\n{}",
            out.code
        );
    }

    #[test]
    fn capitalized_and_lowercase_names_both_reparse() {
        // The component name policy is PascalCase of the file stem regardless of the author's casing;
        // a lowercase authoring name must still yield a valid module.
        let lower = "export default function about() {\n  return <div>x</div>;\n}\n";
        let out = compile(lower, "about.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        assert_well_formed_module(&out.code);
        assert!(out.code.contains("export default About;"), "wrong default export; got: {}", out.code);
    }

    #[test]
    fn jsx_inline_server_fn_in_component_body_absent_from_client_and_map() {
        // FEATURE: server fn IN a component (JSX). A `'use server'` fn declared INSIDE the component
        // body is extracted to the backend, its call site rewritten to the API call, and its body
        // ABSENT from BOTH the client code AND the client source map (no secret leak).
        let source = "export default function Card() {\n\
  async function loadUser(id: number) {\n\
    'use server';\n\
    return secretDb.users.find(id);\n\
  }\n\
  const onClick = () => loadUser(1);\n\
  return <button onClick={onClick}>go</button>;\n\
}\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);

        // A server module carries the lifted body.
        let server_module = out.server_module.expect("expected a server module for the inline fn");
        assert!(
            server_module.contains("secretDb.users.find") || server_module.contains("__server_loadUser"),
            "server module did not carry the lifted inline fn; got:\n{server_module}"
        );

        // The client code re-parses (as TSX — the emitted `.tsx` client is TypeScript, TS-erased
        // downstream by the bundler), calls the server route, and does NOT contain the body.
        let allocator = Allocator::default();
        let parsed = JsParser::new(&allocator, &out.code, SourceType::tsx()).parse();
        assert!(
            parsed.errors.is_empty(),
            "emitted client did not re-parse as TSX: {:?}\n{}",
            parsed.errors,
            out.code
        );
        assert!(
            out.code.contains("fetch('/__server/loadUser'"),
            "inline server call not rewritten to the API call; got:\n{}",
            out.code
        );
        assert!(
            !out.code.contains("secretDb.users.find"),
            "SECURITY: inline server body leaked into the JSX client; got:\n{}",
            out.code
        );

        // The client source map does NOT embed the server body either.
        let map = out.map.expect("expected a client source map");
        assert!(
            !map.contains("secretDb.users.find"),
            "SECURITY: inline server body leaked into the client source map; got:\n{map}"
        );
    }

    // --- React-compat mode -----------------------------------------------------
    // A plain React component (hooks + JSX) compiles to Angular Ivy through the same backend.

    #[test]
    fn react_use_state_component_compiles_to_ivy() {
        // `useState` lowers to a signal, the setter to `.set`/`.update`, the read auto-calls, and the
        // whole thing compiles to a valid `ɵɵdefineComponent`.
        let source = "import { useState } from 'react';\n\
export default function Counter() {\n\
  const [count, setCount] = useState(0);\n\
  const inc = () => setCount(prev => prev + 1);\n\
  return <button onClick={inc}>{count}</button>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The react `react` import is gone, `useState` lowered to a signal.
        assert!(!code.contains("from 'react'"), "react import survived; got: {code}");
        assert!(code.contains("count = signal(0)"), "useState not lowered to signal; got: {code}");
        // The functional updater lowered to `.update`.
        assert!(
            code.contains("count.update(prev => prev + 1)"),
            "setCount(prev=>…) not lowered to .update; got: {code}"
        );
        // The template read auto-calls the signal.
        assert!(code.contains("ctx.count()"), "signal read not auto-called; got: {code}");
        // `signal` is imported from @angular/core.
        assert!(core_import_binds(code, "signal"), "signal not imported; got: {code}");
    }

    #[test]
    fn react_full_component_use_state_effect_memo_props_compiles_to_ivy() {
        // The headline acceptance: a full small React component using useState + useEffect + useMemo
        // + props + JSX compiles to a valid `ɵɵdefineComponent`.
        let source = "import { useState, useEffect, useMemo } from 'react';\n\
export default function Counter({ start, step = 1 }) {\n\
  const [count, setCount] = useState(start);\n\
  const doubled = useMemo(() => count * 2, [count]);\n\
  useEffect(() => { console.log(count); }, [count]);\n\
  const inc = () => setCount(prev => prev + step);\n\
  return <button onClick={inc}>{count} / {doubled}</button>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // useState -> signal, useMemo -> computed, useEffect -> effect, all imported in one merge.
        assert!(code.contains("count = signal(start"), "useState not lowered; got: {code}");
        assert!(code.contains("doubled = computed("), "useMemo not lowered; got: {code}");
        assert!(code.contains("effect(() =>"), "useEffect not lowered to effect; got: {code}");
        assert!(!code.contains("useEffect"), "useEffect leaked; got: {code}");
        assert!(!code.contains("useMemo"), "useMemo leaked; got: {code}");
        for name in ["signal", "computed", "effect", "input"] {
            assert!(core_import_binds(code, name), "`{name}` not imported; got: {code}");
        }
        // Props lowered to signal inputs and land in the component `inputs`.
        assert!(code.contains("start = input()"), "prop `start` not an input(); got: {code}");
        assert!(code.contains("step = input(1)"), "prop `step` default lost; got: {code}");
        assert!(code.contains("inputs"), "no inputs map; got: {code}");
        // The template reads auto-call the signals (count + computed memo).
        assert!(code.contains("ctx.count()"), "count read not auto-called; got: {code}");
        assert!(code.contains("ctx.doubled()"), "doubled read not auto-called; got: {code}");
    }

    // --- adversarial regression repros (full pipeline → parseable Ivy) --------

    #[test]
    fn repro1_use_ref_in_use_memo_compiles_to_parseable_ivy() {
        // BUG 1 end-to-end: `useRef` inside `useMemo` used to panic `apply_edits` (overlapping edits).
        // It must now compile to a single parseable `ɵɵdefineComponent`.
        let source = "import { useRef, useMemo } from 'react';\n\
export default function W() {\n\
  const r = useRef(0);\n\
  const m = useMemo(() => r.current + 1, []);\n\
  return <div>{m}</div>;\n\
}\n";
        let out = compile(source, "w.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code); // exactly one `export default`, re-parses as a valid ES module
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The `.current` read composed with the `useMemo`→`computed` rewrite (no overlap panic).
        assert!(code.contains("computed(() => r() + 1)"), "useRef-in-useMemo not composed; got: {code}");
        assert!(!code.contains("useMemo") && !code.contains("useRef"), "hook leaked; got: {code}");
    }

    #[test]
    fn repro2_use_callback_setter_compiles_to_parseable_ivy_with_setter_rewritten() {
        // BUG 2 end-to-end: the useCallback unwrap emitted a stray trailing `)` (unparseable) and did
        // not rewrite the inner setter. It must now compile to a single parseable `ɵɵdefineComponent`
        // with `setVal(...)` rewritten to `val.set(...)`.
        let source = "import { useState, useCallback } from 'react';\n\
export default function Field() {\n\
  const [val, setVal] = useState('');\n\
  const onInput = useCallback((e) => setVal(e.target.value), []);\n\
  return <input value={val} onInput={onInput} />;\n\
}\n";
        let out = compile(source, "field.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The inner setter was rewritten and the unwrap left no stray paren.
        assert!(code.contains("val.set(e.target.value)"), "inner setter not rewritten; got: {code}");
        assert!(!code.contains("value));"), "stray trailing paren leaked; got: {code}");
        assert!(!code.contains("useCallback"), "useCallback leaked; got: {code}");
    }

    #[test]
    fn repro3_body_signal_reads_auto_called_in_computed_and_effect() {
        // BUG 3 end-to-end: a useState signal read inside a `useMemo`/`useEffect` body must be
        // auto-called so the computed is not `NaN` and the effect actually tracks/re-runs.
        let source = "import { useState, useMemo, useEffect } from 'react';\n\
export default function Doubler() {\n\
  const [count, setCount] = useState(0);\n\
  const doubled = useMemo(() => count * 2, [count]);\n\
  useEffect(() => console.log(count), [count]);\n\
  return <div>{doubled}</div>;\n\
}\n";
        let out = compile(source, "doubler.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The signal read is auto-called INSIDE both the emitted computed and effect bodies.
        assert!(code.contains("computed(() => count() * 2)"), "computed body read not called; got: {code}");
        assert!(code.contains("effect(() => console.log(count()))"), "effect body read not called; got: {code}");
    }

    #[test]
    fn react_use_effect_deps_are_dropped() {
        let source = "import { useState, useEffect } from 'react';\n\
export default function Logger() {\n\
  const [n, setN] = useState(0);\n\
  useEffect(() => { document.title = String(n); }, [n]);\n\
  return <div>{n}</div>;\n\
}\n";
        let out = compile(source, "logger.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The effect body survived; the deps array did not become a second effect argument.
        assert!(code.contains("effect(() =>"), "no effect; got: {code}");
        assert!(
            !code.contains("], [n])") && !code.contains(", [n])"),
            "effect deps array not dropped; got: {code}"
        );
    }

    // --- props -> input() (BOTH react and our .tsx) ----------------------------

    #[test]
    fn tsx_destructured_props_lower_to_signal_inputs() {
        // NOT react mode (no hooks/imports): our plain `.tsx` must ALSO lower a destructured props
        // parameter to signal inputs (the front-end used to drop the parameter entirely).
        let source = "export default function Greeting({ name, exclaim = false }) {\n\
  return <p>{name}</p>;\n\
}\n";
        let out = compile(source, "greeting.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;

        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // Each prop became a signal input(); the default is preserved.
        assert!(code.contains("name = input()"), "prop `name` not input(); got: {code}");
        assert!(code.contains("exclaim = input(false)"), "prop `exclaim` default lost; got: {code}");
        // The inputs reach the component metadata.
        assert!(code.contains("inputs"), "no inputs map; got: {code}");
        assert!(core_import_binds(code, "input"), "input not imported; got: {code}");
        // The `{name}` template read auto-calls the input signal.
        assert!(code.contains("ctx.name()"), "prop read not auto-called; got: {code}");
    }

    #[test]
    fn tsx_props_inputs_appear_in_component_inputs_map() {
        // Verify (by PARSING the emitted module) that a lowered prop appears as a declared
        // `input` binding at module scope inside the wrapper, and the `inputs` metadata names it.
        let source = "export default function Card({ title }) {\n  return <h1>{title}</h1>;\n}\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        // The Ivy `inputs` metadata names `title`.
        assert!(
            code.contains("inputs") && code.contains("title"),
            "title not surfaced as an input; got: {code}"
        );
        // The synthesized declaration is present.
        assert!(code.contains("title = input()"), "title input declaration missing; got: {code}");
    }

    #[test]
    fn tsx_input_body_destructure_lowers_each_property_to_an_input() {
        // FEATURE: object destructuring → inputs (BODY form). A component that destructures
        // `input<Props>()` inside its body — `const { name, age = 0, label: caption } = input<Props>()`
        // — lowers each property to an INDIVIDUAL input through the SAME shared expansion the `.treaty`
        // path uses, including shorthand, default, and rename.
        let source = "export default function Card() {\n\
  const { name, age = 0, label: caption } = input<Props>();\n\
  return <div>{name()} {age()} {caption()}</div>;\n\
}\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        // The body expands to one `input()` per property (no surviving object destructure).
        assert!(code.contains("const name = input()"), "no expanded `name`; got: {code}");
        assert!(code.contains("const age = input(0)"), "`age` default lost; got: {code}");
        assert!(code.contains("const caption = input()"), "rename local `caption` lost; got: {code}");
        assert!(!code.contains("const { name"), "object destructure survived; got: {code}");
        // The renamed property keeps its PUBLIC `label` name in the Ivy inputs map (the three-element
        // `[flags, publicName, declaredName]` form).
        assert!(code.contains("\"label\""), "rename public name `label` missing; got: {code}");
        assert!(code.contains("\"caption\""), "rename declared name `caption` missing; got: {code}");
    }

    #[test]
    fn tsx_props_param_rename_carries_public_alias() {
        // FEATURE: object destructuring → inputs (PARAM form, rename). A renamed destructured prop
        // `function Card({ label: caption })` keeps `caption` as the runtime signal / class property
        // and `label` as the PUBLIC input name a parent binds to.
        let source = "export default function Card({ label: caption }: Props) {\n\
  return <div>{caption()}</div>;\n\
}\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        // The synthesized declaration aliases to the public name, and the inputs map records both.
        assert!(
            code.contains("input(undefined, { alias: \"label\" })"),
            "rename alias option missing on the synthesized input; got: {code}"
        );
        assert!(
            code.contains("\"label\"") && code.contains("\"caption\""),
            "inputs map missing the rename public/declared name pair; got: {code}"
        );
    }

    #[test]
    fn non_destructured_props_param_reports_a_diagnostic() {
        // A bare `(props)` parameter cannot be enumerated into discrete inputs — surface a clear
        // diagnostic rather than silently dropping it or mis-compiling.
        let source = "export default function Widget(props) {\n  return <div>{props.label}</div>;\n}\n";
        let out = compile(source, "widget.tsx");
        assert!(
            out.errors.iter().any(|e| e.contains("non-destructured props")),
            "expected a non-destructured-props diagnostic; got: {:?}",
            out.errors
        );
        // It still compiles to a component (does not hard-fail).
        assert!(out.code.contains(DEFINE), "no defineComponent; got: {}", out.code);
    }

    #[test]
    fn react_imports_do_not_duplicate_angular_core_import() {
        // When the author ALSO hand-wrote an `@angular/core` import, the React/signals lowering must
        // MERGE the needed primitives into it — exactly one `@angular/core` import, no duplicate.
        let source = "import { inject } from '@angular/core';\n\
import { useState, useEffect } from 'react';\n\
export default function App() {\n\
  const [n, setN] = useState(0);\n\
  useEffect(() => { track(n); }, [n]);\n\
  return <div>{n}</div>;\n\
}\n";
        let out = compile(source, "app.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);

        // Count the NAMED `@angular/core` imports off the parsed AST — there must be exactly one (the
        // merged authoring import). The render3 backend additionally emits an `import * as i0 from
        // "@angular/core"` namespace import; that is the backend's and is a distinct, expected form,
        // so we count only braced/named specifier imports (the authoring-level merge target).
        let allocator = Allocator::default();
        let module_type = SourceType::default().with_module(true).with_typescript(true);
        let parsed = JsParser::new(&allocator, code, module_type).parse();
        assert!(parsed.errors.is_empty(), "did not parse: {code}");
        let named_core_imports = parsed
            .program
            .body
            .iter()
            .filter(|s| {
                let Statement::ImportDeclaration(i) = s else { return false };
                i.source.value == "@angular/core"
                    && i.specifiers.as_ref().is_some_and(|specs| {
                        specs.iter().any(|sp| {
                            matches!(sp, oxc_ast::ast::ImportDeclarationSpecifier::ImportSpecifier(_))
                        })
                    })
            })
            .count();
        assert_eq!(
            named_core_imports, 1,
            "expected exactly one merged named @angular/core import, found {named_core_imports}; got: {code}"
        );
        // The merged import binds both the author's `inject` and the lowered `signal` + `effect`.
        for name in ["inject", "signal", "effect"] {
            assert!(core_import_binds(code, name), "`{name}` not in merged import; got: {code}");
        }
    }

    // --- GAP 1: control-flow CONDITION signal reads auto-called (end to end) ----

    #[test]
    fn react_if_condition_signal_read_is_auto_called_end_to_end() {
        // GAP 1: the `examples/treaty-shadcn/src/card.tsx` shape. `{expanded && <p/>}` lowers to an
        // `@if (expanded)` whose head reads a WritableSignal — bare, the `@if` never toggles. The head
        // MUST auto-call the signal: the emitted Ivy conditional binds `ctx.expanded()`, not the bare
        // (always-truthy) `ctx.expanded`.
        let source = "import { useState } from 'react';\n\
export default function Card({ title, description }) {\n\
  const [expanded, setExpanded] = useState(false);\n\
  const toggle = () => setExpanded((v) => !v);\n\
  return (\n\
    <div className=\"card\">\n\
      <h3>{title}</h3>\n\
      <button className=\"card-toggle\" onClick={toggle}>{expanded ? 'Hide' : 'Show'} details</button>\n\
      {expanded && <p className=\"card-description\">{description}</p>}\n\
    </div>\n\
  );\n\
}\n";
        let out = compile(source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // The `@if` condition reads the CALLED signal in the Ivy conditional binding.
        assert!(
            code.contains("ctx.expanded()"),
            "@if condition signal read not auto-called (the toggle bug); got: {code}"
        );
        // And it is NOT the bare (always-truthy) function reference: there is no `ctx.expanded ?`
        // / `ctx.expanded :` / `ctx.expanded ;` / `ctx.expanded )` form (a bare read in the binding).
        for bare in ["ctx.expanded ?", "ctx.expanded :", "ctx.expanded;", "ctx.expanded)"] {
            assert!(
                !code.contains(bare),
                "bare (uncalled) `expanded` signal still in a conditional binding (`{bare}`); got: {code}"
            );
        }
    }

    // --- GAP 2: inline-arrow event handlers unwrapped + setter rewritten (e2e) --

    #[test]
    fn react_inline_arrow_setter_handler_unwraps_and_rewrites_to_update() {
        // GAP 2: `onClick={() => setCount(c => c + 1)}` must NOT bind a returned function and must
        // rewrite the React setter. The emitted listener action is `count.update(c => c + 1)` — no
        // wrapping arrow (no `() =>`), and no `setCount` (the setter is gone).
        let source = "import { useState } from 'react';\n\
export default function Counter() {\n\
  const [count, setCount] = useState(0);\n\
  return <button onClick={() => setCount(c => c + 1)}>{count}</button>;\n\
}\n";
        let out = compile(source, "counter.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");

        // The listener action is the unwrapped, setter-rewritten body.
        assert!(
            code.contains("count.update(c => c + 1)"),
            "inline-arrow setter not rewritten to count.update; got: {code}"
        );
        // No wrapping arrow leaked into the listener (it would only RETURN a function).
        assert!(
            !code.contains("ctx.setCount") && !code.contains("setCount("),
            "React setter `setCount` survived in the listener; got: {code}"
        );
        // A real listener instruction was emitted for the click.
        assert!(
            code.contains("\u{0275}\u{0275}listener") || code.contains("\u{0275}\u{0275}domListener"),
            "no listener instruction for onClick; got: {code}"
        );
    }

    #[test]
    fn react_inline_arrow_event_param_handler_unwraps_and_maps_dollar_event() {
        // An inline arrow taking the event maps the param to `$event`, unwraps, and rewrites the
        // setter: `onInput={(e) => setName(e.target.value)}` → `name.set($event.target.value)`.
        let source = "import { useState } from 'react';\n\
export default function Field() {\n\
  const [name, setName] = useState('');\n\
  return <input onInput={(e) => setName(e.target.value)} />;\n\
}\n";
        let out = compile(source, "field.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "no defineComponent; got: {code}");
        assert!(
            code.contains("name.set($event.target.value)"),
            "inline-arrow event handler not unwrapped/$event-mapped/setter-rewritten; got: {code}"
        );
        assert!(!code.contains("setName"), "React setter survived; got: {code}");
    }

    // --- the real shadcn example files compile correctly (GAP 1 + GAP 2) -------

    /// Read a `examples/treaty-shadcn/src/<name>` file relative to the repo root (three levels up from
    /// this crate's `libs/authoring/rust` manifest dir).
    fn read_shadcn_example(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../examples/treaty-shadcn/src")
            .join(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read example {}: {e}", path.display()))
    }

    #[test]
    fn real_card_tsx_example_if_condition_is_auto_called() {
        // The literal `examples/treaty-shadcn/src/card.tsx`, compiled through the JSX front-end, must
        // produce a toggling `@if`: its condition reads the CALLED `expanded` signal (`ctx.expanded()`),
        // not the always-truthy bare function — the GAP 1 fix, verified on the shipped source.
        let source = read_shadcn_example("card.tsx");
        let out = compile(&source, "card.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        assert_well_formed_module(code);
        assert!(code.contains(DEFINE), "card.tsx did not compile to a component; got: {code}");

        // GAP 1: the `{expanded && <p/>}` conditional binds the CALLED signal.
        assert!(
            code.contains("ctx.expanded()"),
            "card.tsx @if condition not auto-called (toggle bug); got: {code}"
        );
        for bare in ["ctx.expanded ?", "ctx.expanded :", "ctx.expanded;", "ctx.expanded)"] {
            assert!(
                !code.contains(bare),
                "card.tsx still binds a bare (always-truthy) `expanded` (`{bare}`); got: {code}"
            );
        }
        // The `useState` lowered to a signal and the named `toggle` handler's setter became `.update`.
        assert!(code.contains("expanded = signal(false)"), "useState not lowered; got: {code}");
        assert!(
            code.contains("expanded.update("),
            "setExpanded(v => !v) not lowered to update; got: {code}"
        );
    }

    #[test]
    fn real_alert_tsx_example_negated_if_condition_is_auto_called() {
        // `examples/treaty-shadcn/src/alert.tsx`: `{!dismissed && <div/>}` → `@if (!dismissed())`, and
        // the nested `{isDismissible && <button/>}` → `@if (isDismissible())`. Both heads auto-call.
        let source = read_shadcn_example("alert.tsx");
        let out = compile(&source, "alert.tsx");
        assert!(out.errors.is_empty(), "unexpected errors: {:?}", out.errors);
        let code = &out.code;
        // alert.tsx carries TS-only syntax in its body (a top-level `type AlertType = …` alias and a
        // `'info' as AlertType` prop default that the front-end splices verbatim), so it must re-parse
        // as a TYPESCRIPT module (the plain-JS `assert_well_formed_module` would reject the alias —
        // that is an orthogonal, pre-existing TS-in-body gap, not a GAP-1/2 regression).
        assert_jsx_client_parses(code);
        assert!(code.contains(DEFINE), "alert.tsx did not compile to a component; got: {code}");

        // The dismissed-state signal read is CALLED in its (outer) conditional binding.
        assert!(
            code.contains("dismissed()"),
            "alert.tsx `!dismissed` condition not auto-called; got: {code}"
        );
        assert!(
            !code.contains("!ctx.dismissed ?") && !code.contains("ctx.dismissed ?"),
            "alert.tsx still binds a bare (always-truthy) `dismissed`; got: {code}"
        );
        // The `isDismissible` prop input read is CALLED in its (nested) conditional binding. The
        // nested embedded view references it through the restored context (`ctx_r1.isDismissible()`).
        assert!(
            code.contains("isDismissible()"),
            "alert.tsx `isDismissible` condition not auto-called; got: {code}"
        );
        // `useState` lowered + the named `dismiss` setter became `.set(true)`.
        assert!(code.contains("dismissed = signal(false)"), "useState not lowered; got: {code}");
        assert!(code.contains("dismissed.set(true)"), "setDismissed(true) not lowered; got: {code}");
    }
}
