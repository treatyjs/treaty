#[macro_use]
extern crate napi_derive;

use treaty_file_routing::{
    emit_ts as r3_emit_routes_ts, generate_routing as r3_generate_routing, referenced_files as r3_referenced_files,
    FileRoutingConfig, PartialFileRoutingConfig, RealFsDirTree,
};
use treaty_ivy::compile::compile_component as r3_compile_component;
use treaty_ivy::linker::link_partial as r3_link_partial;
use treaty_ivy::source_compile::compile_component_source_with_map as r3_compile_component_source_with_map;

/// Normalize render3's empty-string "no map" sentinel into `None` (it never emits `{}`).
fn map_or_none(map: String) -> Option<String> {
    if map.trim().is_empty() {
        None
    } else {
        Some(map)
    }
}

/// Result of compiling a component template to Ivy.
#[napi(object)]
pub struct CompiledComponent {
    /// The emitted JavaScript (the `ɵɵdefineComponent({...})` definition).
    pub code: String,
    /// Parse/transform diagnostics (empty on success).
    pub errors: Vec<String>,
    /// The additive Source Map v3 JSON mapping `code` back to the original source, or `undefined`
    /// when the underlying path produced no map (e.g. the template-only `compileComponent` entry).
    pub map: Option<String>,
}

/// Compile an Angular component template directly to Ivy via the Rust/OXC `render3` compiler.
///
/// `template` is the HTML template source, `selector` the component selector
/// (e.g. `"app-hello"`), and `class_name` the component class identifier.
#[napi]
pub fn compile_component(
    template: String,
    selector: String,
    class_name: String,
) -> CompiledComponent {
    let result = r3_compile_component(&template, &selector, &class_name);
    CompiledComponent {
        code: result.code,
        errors: result.errors,
        // The template-only entry does not run the source-map pipeline.
        map: None,
    }
}

/// Compile an Angular `@Component`/`@Directive` class directly from its TypeScript SOURCE.
///
/// `source` is the full TS file (or snippet) containing exactly one decorated class. Returns the
/// emitted `ɵɵdefineComponent({...})` definition plus the additive Source Map v3 JSON (`map`), or a
/// `CompiledComponent` carrying a descriptive error for shapes the source front-end does not yet
/// support (providers, queries, host bindings, `templateUrl`, multi-class files, etc.).
#[napi]
pub fn compile_component_source(source: String) -> CompiledComponent {
    let result = r3_compile_component_source_with_map(&source, "component.js", "component.ts");
    CompiledComponent {
        code: result.code,
        errors: result.errors,
        map: map_or_none(result.map),
    }
}

/// Link an Angular **partial-declaration** module directly to its full AOT form.
///
/// Published Angular libraries ship *partial*-compiled: classes emit `ɵɵngDeclare*({...})` calls
/// instead of the full `ɵɵdefine*({...})`. This is the Rust Angular Linker: it rewrites every
/// linkable `ɵɵngDeclare*` call (the DI + pipe family — `ɵɵngDeclareFactory`/`Injectable`/
/// `Injector`/`NgModule`/`Pipe`, plus dropping the dev-only `ɵɵngDeclareClassMetadata`) into the
/// corresponding `ɵɵdefine*` call by driving the SAME render3 emit fed from the declaration object.
///
/// `code` is the module source (typically a `.mjs` fesm chunk); `file_name` selects the parse mode.
/// The returned `code` is byte-identical to the input outside the rewritten call spans; `errors`
/// carries diagnostics for any declaration that could not be linked (its call is left untouched).
#[napi]
pub fn link_partial(code: String, file_name: String) -> CompiledComponent {
    let result = r3_link_partial(&code, &file_name);
    CompiledComponent {
        code: result.code,
        errors: result.errors,
        // Linking is a span rewrite of an existing module; no source map is produced here.
        map: None,
    }
}

/// Compile a `.treaty` single-file component directly from its source.
///
/// `source` is the full `.treaty` file contents and `file_name` its path/name (used for
/// diagnostics). Returns the emitted `ɵɵdefineComponent({...})` definition, or a
/// `CompiledComponent` carrying descriptive errors.
#[napi]
pub fn compile_treaty_file(source: String, file_name: String) -> CompiledComponent {
    // Emit the additive v3 map alongside the component, embedding the original `.treaty` source as
    // `sourcesContent` (named by `file_name`). This entry is server-block-unaware (it compiles the
    // raw source verbatim); the server-block-aware `.treaty` path with body redaction is reached
    // through the unified `compile` / `compile_many` entries.
    let (result, map) =
        rust_authoring::sfc::compile_treaty_file_with_map(&source, &file_name, &file_name, &source);
    CompiledComponent {
        code: result.code,
        errors: result.errors,
        map,
    }
}

/// Result of the unified per-file authoring compile ([`compile`]).
#[napi(object)]
pub struct CompiledAuthoring {
    /// The compiled client module (the `ɵɵdefineComponent({...})` output, or — for a faithful
    /// pass-through `.ts`/`.json`/etc. — the original source unchanged).
    pub code: String,
    /// The generated backend module when the source declared a `server { … }` block; otherwise
    /// `undefined`.
    pub server_module: Option<String>,
    /// Parse/transform diagnostics (empty on success).
    pub errors: Vec<String>,
    /// The additive Source Map v3 JSON mapping `code` back to the original authoring source, or
    /// `undefined` when the routed front-end produced no map. CLIENT PRIVACY: when a `server { … }`
    /// block was present, every lifted server-fn body has been redacted from the map's
    /// `sourcesContent` before this field is populated.
    pub map: Option<String>,
}

/// Unified per-file authoring compile: route `source` to the right front-end by `file_name`'s
/// extension and return the compiled client module (+ optional server module + diagnostics).
///
/// Extension routing (via `rust_authoring`'s `AuthoringRegistry`):
///   * `.treaty` → the `.treaty` SFC front-end.
///   * `.tsx` / `.tjsx` → the JSX front-end, which handles BARE JSX
///     (`export default function App() { return <div/> }`) as well as `@Component` JSX — closing
///     the gap left by [`compile_component_source`], which only accepts `@Component`.
///   * `.ts` → the base-Angular front-end: `@Component` classes compile to `ɵɵdefineComponent`
///     (server-block aware); `@Directive`/`@Pipe`/`@Injectable`/`@NgModule` and plain non-Angular
///     modules pass through unchanged.
///   * any other extension → passed through unchanged.
#[napi]
pub fn compile(source: String, file_name: String) -> CompiledAuthoring {
    let result = rust_authoring::authoring::compile_file(&source, &file_name);
    CompiledAuthoring {
        code: result.code,
        server_module: result.server_module,
        errors: result.errors,
        map: result.map,
    }
}

/// One file to compile in a [`compile_many`] batch.
#[napi(object)]
pub struct AuthoringFile {
    /// Caller-supplied identifier echoed back on the result (the file path/name). It is also used as
    /// the routing `file_name` (its extension selects the front-end), so it must carry the real
    /// extension (e.g. `"src/App.tsx"`, `"greeting.treaty"`).
    pub id: String,
    /// The full source of the file.
    pub code: String,
}

/// One compiled file in a [`compile_many`] batch — the [`CompiledAuthoring`] result tagged with its
/// originating [`AuthoringFile::id`].
#[napi(object)]
pub struct CompiledAuthoringEntry {
    /// The `id` of the input file this result corresponds to (echoed verbatim).
    pub id: String,
    /// The compiled client module (or faithful pass-through source).
    pub code: String,
    /// The generated backend module when the source declared a `server { … }` block; otherwise
    /// `undefined`.
    pub server_module: Option<String>,
    /// Parse/transform diagnostics (empty on success).
    pub errors: Vec<String>,
    /// The additive Source Map v3 JSON mapping `code` back to the original authoring source, or
    /// `undefined` when the routed front-end produced no map. Server-fn bodies are redacted from the
    /// map's `sourcesContent` (client privacy), exactly as for the single-file [`compile`] entry.
    pub map: Option<String>,
}

/// Compile many authoring files IN PARALLEL across all available cores.
///
/// Each file is routed and compiled by [`rust_authoring::authoring::compile_file`] exactly as the
/// single-file [`compile`] entry does — `.treaty` / `.tsx` / `.tjsx` / `.ts` to their front-ends,
/// any other extension passed through unchanged. The work is fanned out with `rayon` because each
/// compile is CPU-bound, fully synchronous, and self-contained: every call builds its own oxc
/// `Allocator`/arena and shares no mutable global state, so the compiles run safely in parallel on
/// separate worker threads (the compiler's `thread_local!` scratch tables are per-thread and reset
/// at the start of each pass).
///
/// Results are returned in INPUT ORDER regardless of completion order, each tagged with its input
/// `id`.
#[napi]
pub fn compile_many(files: Vec<AuthoringFile>) -> Vec<CompiledAuthoringEntry> {
    use rayon::prelude::*;

    files
        .into_par_iter()
        .map(|file| {
            let result = rust_authoring::authoring::compile_file(&file.code, &file.id);
            CompiledAuthoringEntry {
                id: file.id,
                code: result.code,
                server_module: result.server_module,
                errors: result.errors,
                map: result.map,
            }
        })
        .collect()
}

/// The generated file-routing **virtual module** plus its watch dependency set.
///
/// Produced by [`generate_routes`] for a bundler plugin to serve as an in-memory
/// module during a build (no checked-in / prebuilt `routes.ts`). `code` is the
/// emitted TypeScript routes module — byte-identical to the `treaty-file-routing`
/// CLI `--emit ts` output because both call the SAME pure-core emitter
/// ([`treaty_file_routing::emit_ts`]). `files` are the tree-relative route entry
/// files the module's lazy `import(...)` loaders reference, for the bundler to
/// register as watch dependencies so editing a route re-runs the virtual module.
#[napi(object)]
pub struct GeneratedRoutes {
    /// The emitted TypeScript routes module (`export const routes`, `export default
    /// routes`, `export const federationRemotes`).
    pub code: String,
    /// Tree-relative route entry files the emitted module references (watch deps),
    /// in route depth-first then federation order, de-duplicated.
    pub files: Vec<String>,
}

/// The `config_json` knobs accepted by [`generate_routes`]: the
/// [`PartialFileRoutingConfig`] routing fields (camelCase: `routesDir`, `apiDir`,
/// `dynamicSegmentStyle` (`"bracket"`/`"colon"`), `federation`, …) plus the
/// emit-only `importBase` prefix prepended to each loader `import(...)` path.
///
/// `importBase` is captured here rather than on [`PartialFileRoutingConfig`]
/// because it governs *emission* (where the virtual module sits relative to the
/// route files), not how the tree is *interpreted*. Absent fields fall back to
/// the routing defaults / the `../../` base used by the CLI.
#[derive(serde::Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GenerateRoutesOptions {
    /// Overrides overlaid onto the file-routing defaults (flattened so the JSON
    /// is a flat object of `routesDir` / `apiDir` / `dynamicSegmentStyle` / … ).
    #[serde(flatten)]
    config: PartialFileRoutingConfig,
    /// Loader `import(...)` path prefix for the emitted module's entry files.
    import_base: Option<String>,
}

/// Generate the file-routing module for `root_dir` DURING a build, as a virtual
/// module — the bundler-plugin shim over the pure [`treaty_file_routing`] core.
///
/// `root_dir` is the project root that CONTAINS the configured `routes/` and
/// `api/` directories. `config_json` is a JSON object of the
/// [`GenerateRoutesOptions`] knobs (`routesDir`, `apiDir`, `dynamicSegmentStyle`
/// `"bracket"`/`"colon"`, `federation`, `importBase`, …); pass `""` or `"{}"` for
/// the defaults.
///
/// Drives the SAME pipeline as the CLI: a [`RealFsDirTree`] over `root_dir` →
/// [`generate_routing`] → [`emit_ts`], so the returned `code` matches
/// `treaty-file-routing --emit ts` byte-for-byte for the same tree + base. The
/// returned `files` are the route entry files the module references, for the
/// plugin to register as watch dependencies.
///
/// [`generate_routing`]: treaty_file_routing::generate_routing
/// [`emit_ts`]: treaty_file_routing::emit_ts
#[napi]
pub fn generate_routes(root_dir: String, config_json: String) -> napi::Result<GeneratedRoutes> {
    let trimmed = config_json.trim();
    let options: GenerateRoutesOptions = if trimmed.is_empty() {
        GenerateRoutesOptions::default()
    } else {
        serde_json::from_str(trimmed)
            .map_err(|e| napi::Error::from_reason(format!("invalid file-routing config JSON: {e}")))?
    };

    let config: FileRoutingConfig = FileRoutingConfig::resolve(options.config);
    // The CLI's default import base; the bundler shim overrides it per its
    // virtual-module location.
    let import_base = options.import_base.as_deref().unwrap_or("../../");

    let tree = RealFsDirTree::new(&root_dir);
    let routing = r3_generate_routing(&config, &tree);

    Ok(GeneratedRoutes {
        code: r3_emit_routes_ts(&routing, import_base),
        files: r3_referenced_files(&routing),
    })
}

/// Execute a Treaty macro through the Nova-backed [`treaty_runtime`] and return its produced value
/// as a JSON string.
///
/// `ts_source` is the TypeScript body of a macro (the top-of-file fenced block in a `.treaty` file).
/// `input_json` is a JSON string injected as the macro's `input` / `__args` globals (pass `"null"`
/// for no input). The returned string is the JSON encoding of the macro's produced value (its
/// default export, explicit `return`, or trailing expression); values with no JSON form encode as
/// `null`.
///
/// Errors (an invalid `input_json`, a transpile/parse failure, or a thrown macro) surface as a
/// rejected JS error carrying the underlying message.
#[napi]
pub fn run_macro(ts_source: String, input_json: String) -> napi::Result<String> {
    let input: serde_json::Value = serde_json::from_str(&input_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid input JSON: {e}")))?;
    let output = treaty_runtime::run_macro(&ts_source, &input)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    serde_json::to_string(output.value())
        .map_err(|e| napi::Error::from_reason(format!("could not encode macro result: {e}")))
}

/// Execute a Treaty server function through the Nova-backed [`treaty_runtime`] and return its result
/// as a JSON string.
///
/// `ts_source` is the TypeScript body of a server function; `args_json` is a JSON string (typically
/// an array of positional arguments) injected as the function's `args` / `__args` globals. The
/// returned string is the JSON encoding of the value the function `return`s (or its trailing
/// expression).
///
/// Errors (an invalid `args_json`, a transpile/parse failure, or a thrown function) surface as a
/// rejected JS error carrying the underlying message.
#[napi]
pub fn run_server_fn(ts_source: String, args_json: String) -> napi::Result<String> {
    let args: serde_json::Value = serde_json::from_str(&args_json)
        .map_err(|e| napi::Error::from_reason(format!("invalid args JSON: {e}")))?;
    let result = treaty_runtime::run_server_fn(&ts_source, &args)
        .map_err(|e| napi::Error::from_reason(e.to_string()))?;
    serde_json::to_string(&result)
        .map_err(|e| napi::Error::from_reason(format!("could not encode server-fn result: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFINE: &str = "\u{0275}\u{0275}defineComponent";

    #[test]
    fn compile_many_returns_each_result_in_input_order() {
        // A mix of front-ends, deliberately ordered so a naive completion-order collect would
        // scramble them: bare JSX, a `.treaty` SFC, a plain pass-through `.ts`, and another JSX.
        let files = vec![
            AuthoringFile {
                id: "Alpha.tsx".to_string(),
                code: "export default function Alpha() {\n  const a = 1;\n  return <div>{a}</div>;\n}\n"
                    .to_string(),
            },
            AuthoringFile {
                id: "greeting.treaty".to_string(),
                code: "const name = 'World';\n<div>{{ name }}</div>".to_string(),
            },
            AuthoringFile {
                id: "util.ts".to_string(),
                code: "export const greet = (n: string) => `hi ${n}`;\n".to_string(),
            },
            AuthoringFile {
                id: "Beta.tsx".to_string(),
                code: "export default function Beta() {\n  const b = 2;\n  return <span>{b}</span>;\n}\n"
                    .to_string(),
            },
        ];

        let out = compile_many(files);

        // One result per input, in the SAME order.
        assert_eq!(out.len(), 4);
        assert_eq!(
            out.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["Alpha.tsx", "greeting.treaty", "util.ts", "Beta.tsx"]
        );
        for entry in &out {
            assert!(entry.errors.is_empty(), "{} had errors: {:?}", entry.id, entry.errors);
        }

        // Each result is exactly what the single-file entry produces for that file — proving the
        // parallel batch is order-preserving and per-file faithful.
        assert!(out[0].code.contains("Alpha_Template"), "Alpha JSX: {}", out[0].code);
        assert!(out[0].code.contains(DEFINE), "Alpha JSX: {}", out[0].code);
        assert!(out[1].code.contains(DEFINE), "treaty: {}", out[1].code);
        assert_eq!(
            out[2].code, "export const greet = (n: string) => `hi ${n}`;\n",
            "plain .ts must pass through unchanged"
        );
        assert!(out[2].server_module.is_none());
        assert!(out[3].code.contains("Beta_Template"), "Beta JSX: {}", out[3].code);
        assert!(out[3].code.contains(DEFINE), "Beta JSX: {}", out[3].code);
    }

    #[test]
    fn compile_many_empty_input_returns_empty() {
        assert!(compile_many(Vec::new()).is_empty());
    }

    #[test]
    fn run_macro_executes_and_returns_json_string() {
        // A TS macro reading its injected input; the addon returns the produced value as JSON text.
        let src = "const n: number = input.n; return { doubled: n * 2 };";
        let out = run_macro(src.to_string(), "{\"n\": 21}".to_string())
            .expect("macro should run");
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid JSON result");
        assert_eq!(value, serde_json::json!({ "doubled": 42 }));
    }

    #[test]
    fn run_macro_surfaces_thrown_error() {
        let err = run_macro(
            "throw new Error('macro blew up');".to_string(),
            "null".to_string(),
        )
        .expect_err("a throwing macro should error");
        assert!(err.reason.contains("macro blew up"), "got: {}", err.reason);
    }

    #[test]
    fn run_macro_rejects_invalid_input_json() {
        let err = run_macro("return 1;".to_string(), "{ not json".to_string())
            .expect_err("invalid input JSON should error");
        assert!(err.reason.contains("invalid input JSON"), "got: {}", err.reason);
    }

    #[test]
    fn run_server_fn_executes_with_args() {
        // A server fn reading positional arguments from `args`; the addon returns its JSON result.
        let src = "const a: number = args[0]; const b: number = args[1]; return a + b;";
        let out = run_server_fn(src.to_string(), "[4, 38]".to_string())
            .expect("server fn should run");
        let value: serde_json::Value = serde_json::from_str(&out).expect("valid JSON result");
        assert_eq!(value, serde_json::json!(42));
    }

    #[test]
    fn run_server_fn_surfaces_thrown_error() {
        let err = run_server_fn(
            "throw new Error('server fn failed');".to_string(),
            "[]".to_string(),
        )
        .expect_err("a throwing server fn should error");
        assert!(err.reason.contains("server fn failed"), "got: {}", err.reason);
    }

    /// The example app's routes root — `examples/file-routed-app` relative to the
    /// addon crate (libs/authoring/node → up four to repo root).
    fn example_routes_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
            .join("examples")
            .join("file-routed-app")
    }

    #[test]
    fn generate_routes_emits_module_and_watch_files() {
        let root = example_routes_root().to_string_lossy().to_string();
        let out = generate_routes(root, String::new()).expect("default config generates");

        // The emitted code is a consumable Angular route module.
        assert!(out.code.contains("export const routes: Routes = ["));
        assert!(out.code.contains("export default routes"));
        assert!(out.code.contains("export const federationRemotes = "));
        assert!(out.code.contains("loadComponent: () => import("));

        // The default import base (../../) prefixes the tree-relative entries.
        assert!(out.code.contains("import(\"../../routes/index.treaty\")"));

        // The watch-dependency list carries the referenced route entry files
        // (tree-relative, no import base), de-duplicated.
        assert!(out.files.iter().any(|f| f == "routes/layout.treaty"));
        assert!(out.files.iter().any(|f| f == "routes/index.treaty"));
        let mut sorted = out.files.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), out.files.len(), "watch files are unique");
    }

    #[test]
    fn generate_routes_honors_config_and_import_base() {
        let root = example_routes_root().to_string_lossy().to_string();
        // Colon style + custom import base + federation off.
        let cfg = r#"{"dynamicSegmentStyle":"colon","federation":false,"importBase":"@routes"}"#;
        let out = generate_routes(root, cfg.to_string()).expect("config generates");

        // Federation off → empty remotes.
        assert!(out.code.contains("export const federationRemotes = [] as const"));
        // Custom import base honored (a single separator inserted).
        assert!(out.code.contains("import(\"@routes/routes/index.treaty\")"));
        // Colon style honored: no surviving bracket dynamic segments in paths.
        assert!(!out.code.contains("path: \"[slug]\""));
    }

    #[test]
    fn generate_routes_rejects_invalid_config_json() {
        let result = generate_routes("whatever".to_string(), "{ not json".to_string());
        let err = match result {
            Ok(_) => panic!("invalid config JSON should error"),
            Err(e) => e,
        };
        assert!(err.reason.contains("invalid file-routing config JSON"), "got: {}", err.reason);
    }

    #[test]
    fn generate_routes_empty_root_yields_empty_module() {
        // A root with no routes/ dir contributes nothing but still emits a
        // well-formed (empty) module rather than failing.
        let out = generate_routes("definitely/not/a/real/path".to_string(), "{}".to_string())
            .expect("missing root generates an empty module");
        assert!(out.code.contains("export const routes: Routes = [\n]"));
        assert!(out.files.is_empty());
    }
}
