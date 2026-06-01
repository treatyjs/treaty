//! `treaty-file-routing` — the runnable CLI front-end for the
//! [`treaty_file_routing`] core.
//!
//! The library crate is pure: [`generate_routing`] turns an injected
//! [`DirTree`] + a [`FileRoutingConfig`] into serde-serializable *data*
//! ([`GeneratedRouting`]). This binary is the thin host that makes the core
//! genuinely runnable end to end against a project on disk: it builds a
//! [`RealFsDirTree`] over a routes-root directory, resolves a config from CLI
//! flags, runs the pipeline, and emits the result either as deterministic
//! pretty JSON or as a ready-to-import TypeScript routes module that a JS/Vite
//! app can consume directly (an Angular `Routes` array of lazy
//! `loadComponent: () => import(...)` boundaries plus the Module Federation
//! remote descriptors).
//!
//! # Usage
//!
//! ```text
//! treaty-file-routing <routes-root> [options]
//!
//!   <routes-root>            Project root that CONTAINS the routes/ and api/
//!                            directories (the dir the config's routes_dir /
//!                            api_dir are resolved against).
//!
//!   --routes-dir <name>      Routes directory name        (default: routes)
//!   --api-dir <name>         Api directory name           (default: api)
//!   --style bracket|colon    Dynamic-segment spelling     (default: bracket)
//!   --no-federation          Disable Module Federation remote emission
//!   --emit json|ts           Output format                (default: json)
//!   --import-base <prefix>   Loader import() path prefix prepended to each
//!                            tree-relative entry file in --emit ts
//!                            (default: ../../)
//!   --out <file>             Write output to <file> instead of stdout
//!   -h, --help               Print this help and exit
//! ```
//!
//! The output is fully deterministic for a given tree + config: the core sorts
//! every directory listing and emits in a stable order, and JSON is serialized
//! with sorted, pretty formatting.

use std::path::PathBuf;
use std::process::ExitCode;

use treaty_file_routing::{
    emit_ts, generate_routing, DynamicSegmentStyle, FileRoutingConfig, GeneratedRouting,
    PartialFileRoutingConfig, RealFsDirTree,
};

/// What to emit on stdout / to `--out`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmitFormat {
    /// Pretty JSON of the whole [`GeneratedRouting`].
    Json,
    /// A TypeScript route module (Angular `Routes` + federation remotes).
    Ts,
}

/// Parsed CLI invocation.
#[derive(Debug, Clone)]
struct Cli {
    /// Project root containing the routes/ and api/ directories.
    routes_root: PathBuf,
    /// Resolved file-routing config the pipeline runs under.
    config: FileRoutingConfig,
    /// Output format.
    emit: EmitFormat,
    /// Path prefix prepended to each entry file for `--emit ts` loaders.
    import_base: String,
    /// Optional output file; `None` writes to stdout.
    out: Option<PathBuf>,
}

/// One-line program name for usage/diagnostics.
const PROG: &str = "treaty-file-routing";

/// Full usage text, printed for `--help` and on argument errors.
const USAGE: &str = "\
treaty-file-routing — run Treaty file-based routing against a directory on disk.

USAGE:
    treaty-file-routing <routes-root> [options]

ARGS:
    <routes-root>            Project root that CONTAINS the routes/ and api/
                             directories.

OPTIONS:
    --routes-dir <name>      Routes directory name        [default: routes]
    --api-dir <name>         Api directory name           [default: api]
    --style <bracket|colon>  Dynamic-segment spelling      [default: bracket]
    --no-federation          Disable Module Federation remote emission
    --emit <json|ts>         Output format                 [default: json]
    --import-base <prefix>   import() path prefix for --emit ts [default: ../../]
    --out <file>             Write output to <file> instead of stdout
    -h, --help               Print this help and exit";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(Some(output)) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        // `--help`: usage already printed to stdout by the parser.
        Ok(None) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{PROG}: error: {err}");
            eprintln!("\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

/// Parse `args`, run the pipeline, and return the rendered output string
/// (already written to `--out` when one is supplied, in which case the returned
/// string is a short confirmation line). Returns `Ok(None)` when `--help` was
/// requested. Kept separate from [`main`] so it is unit-testable.
fn run(args: &[String]) -> Result<Option<String>, String> {
    let Some(cli) = parse_args(args)? else {
        // Help was requested: emit usage to stdout and signal "nothing else".
        println!("{USAGE}");
        return Ok(None);
    };

    let tree = RealFsDirTree::new(&cli.routes_root);
    let routing = generate_routing(&cli.config, &tree);

    let rendered = match cli.emit {
        EmitFormat::Json => render_json(&routing)?,
        // The TypeScript route module is emitted by the SAME pure-core emitter
        // the NAPI binding / bundler plugins call, so the CLI and the build
        // virtual module are byte-identical. See `treaty_file_routing::emit_ts`.
        EmitFormat::Ts => emit_ts(&routing, &cli.import_base),
    };

    match &cli.out {
        Some(path) => {
            std::fs::write(path, rendered.as_bytes())
                .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
            Ok(Some(format!(
                "{PROG}: wrote {} ({} bytes)\n",
                path.display(),
                rendered.len()
            )))
        }
        None => Ok(Some(rendered)),
    }
}

/// Parse the argument vector into a [`Cli`]. Returns `Ok(None)` for `--help`.
///
/// Deterministic and dependency-free: a hand-rolled parser keeps the detached
/// crate free of clap and friends. Unknown flags, missing values, and a missing
/// or duplicated positional `<routes-root>` are hard errors with a clear
/// message; the caller prints usage.
fn parse_args(args: &[String]) -> Result<Option<Cli>, String> {
    let mut routes_root: Option<PathBuf> = None;
    let mut routes_dir: Option<String> = None;
    let mut api_dir: Option<String> = None;
    let mut style: Option<DynamicSegmentStyle> = None;
    let mut federation = true;
    let mut emit = EmitFormat::Json;
    let mut import_base = "../../".to_string();
    let mut out: Option<PathBuf> = None;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(None),
            "--no-federation" => federation = false,
            "--routes-dir" => routes_dir = Some(expect_value(&mut it, "--routes-dir")?),
            "--api-dir" => api_dir = Some(expect_value(&mut it, "--api-dir")?),
            "--import-base" => import_base = expect_value(&mut it, "--import-base")?,
            "--out" => out = Some(PathBuf::from(expect_value(&mut it, "--out")?)),
            "--style" => {
                let v = expect_value(&mut it, "--style")?;
                style = Some(match v.as_str() {
                    "bracket" => DynamicSegmentStyle::Bracket,
                    "colon" => DynamicSegmentStyle::Colon,
                    other => {
                        return Err(format!(
                            "invalid --style {other:?} (expected `bracket` or `colon`)"
                        ));
                    }
                });
            }
            "--emit" => {
                let v = expect_value(&mut it, "--emit")?;
                emit = match v.as_str() {
                    "json" => EmitFormat::Json,
                    "ts" => EmitFormat::Ts,
                    other => {
                        return Err(format!(
                            "invalid --emit {other:?} (expected `json` or `ts`)"
                        ));
                    }
                };
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option {other:?}"));
            }
            positional => {
                if routes_root.is_some() {
                    return Err(format!(
                        "unexpected extra positional argument {positional:?} \
                         (only one <routes-root> is accepted)"
                    ));
                }
                routes_root = Some(PathBuf::from(positional));
            }
        }
    }

    let routes_root = routes_root.ok_or_else(|| "missing required <routes-root> argument".to_string())?;

    let config = FileRoutingConfig::resolve(PartialFileRoutingConfig {
        routes_dir,
        api_dir,
        dynamic_segment_style: style,
        federation: Some(federation),
        ..Default::default()
    });

    Ok(Some(Cli {
        routes_root,
        config,
        emit,
        import_base,
        out,
    }))
}

/// Pull the value following a flag, erroring if the flag was the last token.
fn expect_value<'a>(
    it: &mut impl Iterator<Item = &'a String>,
    flag: &str,
) -> Result<String, String> {
    it.next()
        .map(|s| s.to_string())
        .ok_or_else(|| format!("{flag} requires a value"))
}

/// Serialize the [`GeneratedRouting`] as deterministic pretty JSON (trailing
/// newline so the file/stream ends cleanly).
fn render_json(routing: &GeneratedRouting) -> Result<String, String> {
    let mut json = serde_json::to_string_pretty(routing)
        .map_err(|e| format!("failed to serialize routing as JSON: {e}"))?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route_root() -> PathBuf {
        // tests/ run with CWD = crate dir; the example lives two levels up.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest
            .join("..")
            .join("..")
            .join("examples")
            .join("file-routed-app")
    }

    #[test]
    fn parse_defaults() {
        let cli = parse_args(&["root".to_string()]).unwrap().unwrap();
        assert_eq!(cli.routes_root, PathBuf::from("root"));
        assert_eq!(cli.config.routes_dir, "routes");
        assert_eq!(cli.config.api_dir, "api");
        assert_eq!(cli.config.dynamic_segment_style, DynamicSegmentStyle::Bracket);
        assert!(cli.config.federation);
        assert_eq!(cli.emit, EmitFormat::Json);
        assert_eq!(cli.import_base, "../../");
        assert!(cli.out.is_none());
    }

    #[test]
    fn parse_all_flags() {
        let argv: Vec<String> = [
            "proj", "--routes-dir", "pages", "--api-dir", "server", "--style", "colon",
            "--no-federation", "--emit", "ts", "--import-base", "@app/", "--out", "out.ts",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let cli = parse_args(&argv).unwrap().unwrap();
        assert_eq!(cli.routes_root, PathBuf::from("proj"));
        assert_eq!(cli.config.routes_dir, "pages");
        assert_eq!(cli.config.api_dir, "server");
        assert_eq!(cli.config.dynamic_segment_style, DynamicSegmentStyle::Colon);
        assert!(!cli.config.federation);
        assert_eq!(cli.emit, EmitFormat::Ts);
        assert_eq!(cli.import_base, "@app/");
        assert_eq!(cli.out, Some(PathBuf::from("out.ts")));
    }

    #[test]
    fn help_returns_none() {
        assert!(parse_args(&["--help".to_string()]).unwrap().is_none());
        assert!(parse_args(&["-h".to_string()]).unwrap().is_none());
    }

    #[test]
    fn missing_root_is_error() {
        let err = parse_args(&[]).unwrap_err();
        assert!(err.contains("routes-root"), "got: {err}");
    }

    #[test]
    fn unknown_flag_is_error() {
        let err = parse_args(&["root".to_string(), "--bogus".to_string()]).unwrap_err();
        assert!(err.contains("unknown option"), "got: {err}");
    }

    #[test]
    fn flag_missing_value_is_error() {
        let err = parse_args(&["root".to_string(), "--style".to_string()]).unwrap_err();
        assert!(err.contains("requires a value"), "got: {err}");
    }

    #[test]
    fn bad_style_value_is_error() {
        let err =
            parse_args(&["root".to_string(), "--style".to_string(), "wat".to_string()]).unwrap_err();
        assert!(err.contains("invalid --style"), "got: {err}");
    }

    #[test]
    fn bad_emit_value_is_error() {
        let err =
            parse_args(&["root".to_string(), "--emit".to_string(), "yaml".to_string()]).unwrap_err();
        assert!(err.contains("invalid --emit"), "got: {err}");
    }

    #[test]
    fn extra_positional_is_error() {
        let err = parse_args(&["a".to_string(), "b".to_string()]).unwrap_err();
        assert!(err.contains("extra positional"), "got: {err}");
    }

    #[test]
    fn json_emit_round_trips_and_is_deterministic() {
        let argv: Vec<String> = [route_root().to_string_lossy().to_string()].to_vec();
        let a = run(&argv).unwrap().unwrap();
        let b = run(&argv).unwrap().unwrap();
        assert_eq!(a, b, "JSON emission is deterministic");
        // It parses back into the public type losslessly.
        let parsed: GeneratedRouting = serde_json::from_str(&a).unwrap();
        let direct = generate_routing(&FileRoutingConfig::default(), &RealFsDirTree::new(route_root()));
        assert_eq!(parsed, direct, "CLI JSON matches the library pipeline output");
    }

    #[test]
    fn ts_emit_matches_known_routes_table() {
        let argv: Vec<String> = [
            route_root().to_string_lossy().to_string(),
            "--emit".to_string(),
            "ts".to_string(),
        ]
        .to_vec();
        let ts = run(&argv).unwrap().unwrap();

        // Header + exports present.
        assert!(ts.contains("export const routes: Routes = ["));
        assert!(ts.contains("export default routes"));
        assert!(ts.contains("export const federationRemotes = "));
        assert!(ts.trim_end().ends_with("as const"));

        // The known route table: every entry file appears as a lazy loader with
        // the default ../../ import base, and the nested layout/children shape is
        // reproduced. These mirror the verified e2e route table.
        for (path, entry) in [
            ("path: \"\"", "../../routes/layout.treaty"),
            ("path: \"\"", "../../routes/index.treaty"),
            ("path: \"\"", "../../routes/(marketing)/index.treaty"),
            ("path: \"about\"", "../../routes/(marketing)/about.tjsx"),
            ("path: \"blog\"", "../../routes/blog/layout.treaty"),
            ("path: \"\"", "../../routes/blog/index.treaty"),
            ("path: \"[...path]\"", "../../routes/blog/[...path]/index.treaty"),
            ("path: \"[slug]\"", "../../routes/blog/[slug]/index.treaty"),
            ("path: \"docs/[category]/[page]\"", "../../routes/docs/[category]/[page]/index.tjsx"),
            ("path: \"**\"", "../../routes/not-found.treaty"),
        ] {
            assert!(ts.contains(path), "missing route path line {path:?}\n{ts}");
            let entry_literal = serde_json::to_string(entry).unwrap();
            assert!(
                ts.contains(&format!("import({entry_literal})")),
                "missing loader for {entry:?}\n{ts}"
            );
        }

        // Federation remotes carry the known unique names and entry files.
        for name in [
            "\"root\"", "\"root-index\"", "\"root-marketing\"", "\"about\"", "\"blog\"",
            "\"root-blog\"", "\"path\"", "\"slug\"", "\"docs-category-page\"", "\"not-found\"",
        ] {
            assert!(ts.contains(&format!("\"name\": {name}")), "missing remote {name}");
        }
        assert!(ts.contains("\"exposedModule\": \"./Route\""));

        // Deterministic.
        let again = run(&argv).unwrap().unwrap();
        assert_eq!(ts, again, "TS emission is deterministic");
    }

    #[test]
    fn no_federation_ts_emits_empty_remotes() {
        let argv: Vec<String> = [
            route_root().to_string_lossy().to_string(),
            "--emit".to_string(),
            "ts".to_string(),
            "--no-federation".to_string(),
        ]
        .to_vec();
        let ts = run(&argv).unwrap().unwrap();
        assert!(ts.contains("export const federationRemotes = [] as const"));
    }
}
