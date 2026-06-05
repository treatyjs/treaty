//! The runtime-facing resolution *policy* layer: it turns `(referrer, specifier)` into a concrete
//! load action that a loader ([`crate::node::module_cjs`] / [`crate::node::module_esm`]) can execute.
//!
//! [`crate::node::resolver`] owns the `oxc_resolver` wrapper and the `Resolved` enum — the raw
//! "where does this specifier point" question. This file is the thin layer above it that answers the
//! *next* questions a loader actually needs:
//!
//! * **Referrer-relative base directory.** A loader has a referrer (the importing module's absolute
//!   path, or `None` for the entry script) and a specifier. [`base_dir_for`] derives the directory
//!   `oxc_resolver` should resolve *from*, falling back to the runtime CWD for the entry module.
//! * **`.ts` transpile dispatch.** A resolved file is read with `std::fs` (tenet 4: direct IO, one
//!   syscall) and, when it is TypeScript (`.ts`/`.mts`/`.cts`), threaded through the crate's existing
//!   oxc transpile ([`crate::transpile_ts`]) so the engine only ever sees JavaScript. `.js`/`.mjs`/
//!   `.cjs` pass through untouched (zero-copy: the `String` from `fs::read_to_string` is moved, never
//!   re-allocated). JSON is wrapped into a CommonJS export so `require("./x.json")` yields the parsed
//!   value the way Node does.
//! * **Cache-key derivation.** The loader caches by absolute path; [`cache_key`] canonicalizes the
//!   resolved path so the same file reached via two specifiers shares one cache slot.
//!
//! This layer is deliberately **Nova-free**: it works in plain Rust (`PathBuf`, `String`,
//! [`InstallFn`]) so it is independently unit-testable without standing up an engine, and it holds no
//! `unsafe`. The loaders own the (Nova-touching) step of evaluating the returned source. The uniform
//! [`install`] seam remains an empty object: the resolver policy is an internal service, not an
//! importable module object.

use std::path::{Path, PathBuf};

use nova_vm::ecmascript::{Agent, Object, OrdinaryObject};

use crate::node::core::{InstallError, NodeCtx};
use crate::node::resolver::{ModuleKind, Resolved};
use crate::node::{lookup, GcScope, InstallFn};

/// What a loader must do to satisfy one `require`/`import`, after resolution + (for files) reading
/// and transpiling.
///
/// Borrowing nothing from Nova keeps this fully testable. The loader matches on it: a [`Builtin`]
/// runs the module's `install` (consulting the lazy builtin cache first); a [`File`] evaluates
/// `source` in the appropriate module system and caches the exports under `path`.
///
/// [`Builtin`]: LoadAction::Builtin
/// [`File`]: LoadAction::File
#[derive(Clone)]
pub(crate) enum LoadAction {
    /// A `node:` builtin. Carries the canonical bare specifier and its lazy `install` fn pointer, so
    /// the loader neither re-scans the table nor allocates.
    Builtin {
        /// Canonical bare specifier (e.g. `"path"`), borrowed from the static `BUILTINS` table.
        specifier: &'static str,
        /// The module's lazy `install` entry.
        install: InstallFn,
    },
    /// A file on disk that has been read and (if TypeScript/JSON) lowered to runnable JavaScript.
    File {
        /// Canonical absolute path — the loader's cache key.
        path: PathBuf,
        /// JavaScript source ready for the engine. TS has been transpiled; JSON has been wrapped.
        source: String,
        /// CommonJS vs ESM, so the loader picks the right evaluation path.
        kind: ModuleKind,
    },
}

impl std::fmt::Debug for LoadAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadAction::Builtin { specifier, .. } => {
                f.debug_struct("Builtin").field("specifier", specifier).finish()
            }
            LoadAction::File { path, source, kind } => f
                .debug_struct("File")
                .field("path", path)
                .field("kind", kind)
                .field("source_len", &source.len())
                .finish(),
        }
    }
}

/// Derive the directory `oxc_resolver` should resolve a specifier *from*.
///
/// Node resolves relative/bare specifiers against the importing module's **directory**. When there
/// is no referrer (the entry script, or an `eval`'d snippet), resolution is relative to the runtime
/// CWD. Borrows the referrer's parent without allocating when possible; only the CWD-fallback
/// returns an owned `PathBuf` (cheap clone of an already-owned path).
fn base_dir_for<'a>(referrer: Option<&'a Path>, cwd: &'a Path) -> &'a Path {
    match referrer.and_then(Path::parent) {
        Some(dir) => dir,
        None => cwd,
    }
}

/// Canonicalize a resolved file path into the loader's cache key.
///
/// `std::fs::canonicalize` collapses `.`/`..` and symlinks so two specifiers that name the same file
/// share one cache slot (tenet 3: never load/store a module twice). If canonicalization fails (the
/// path was resolved but is, say, on a filesystem that rejects the query), fall back to the resolved
/// path itself — correctness over a perfect key.
fn cache_key(resolved: &Path) -> PathBuf {
    std::fs::canonicalize(resolved).unwrap_or_else(|_| resolved.to_path_buf())
}

/// Is this extension a TypeScript source the crate transpile must lower before the engine sees it?
fn is_typescript(ext: Option<&str>) -> bool {
    matches!(ext, Some("ts") | Some("mts") | Some("cts"))
}

/// Is this extension JSON, which Node exposes as the parsed value rather than executable source?
fn is_json(ext: Option<&str>) -> bool {
    matches!(ext, Some("json"))
}

/// Turn a resolved-and-read file into runnable JavaScript source.
///
/// * TypeScript (`.ts`/`.mts`/`.cts`) is run through [`crate::transpile_ts`] (oxc) so only the
///   TS-only syntax is stripped; the modern-JS subset Nova accepts is preserved.
/// * JSON is wrapped into a CommonJS assignment, matching Node's `require("./x.json")` semantics.
/// * Everything else (`.js`/`.mjs`/`.cjs`) is already JavaScript: the owned `String` is moved
///   through unchanged (no clone, no re-parse).
fn source_to_js(path: &Path, raw: String) -> Result<String, InstallError> {
    let ext = path.extension().and_then(|e| e.to_str());

    if is_typescript(ext) {
        return crate::transpile_ts(&raw)
            .map_err(|e| InstallError::Io(format!("transpile {}: {e}", path.display())));
    }

    if is_json(ext) {
        // `JSON.parse` keeps Nova as the single source of truth for JSON semantics and is robust to
        // any byte sequence in the file (the source is embedded as a JS string literal). Node's
        // `.json` modules expose the parsed value as the whole export, hence `module.exports = ...`.
        let literal = encode_js_string_literal(&raw);
        return Ok(format!("module.exports = JSON.parse({literal});"));
    }

    // Plain JavaScript: pass the source through untouched (zero-copy move of the read buffer).
    Ok(raw)
}

/// Encode `s` as a double-quoted JavaScript string literal.
///
/// Used to embed arbitrary file bytes (JSON text) safely as a literal that `JSON.parse` then reads.
/// Escapes the JSON/JS-significant characters plus all C0 controls; everything else (including
/// already-valid UTF-8 multibyte sequences) is emitted verbatim, so the common all-ASCII JSON file
/// allocates a string only marginally larger than the input.
fn encode_js_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // U+2028/U+2029 are valid in JSON but terminate a JS string literal; escape them.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Resolve `specifier` (as imported from `referrer`) and, for files, read + lower it to JS.
///
/// This is the one call a loader makes. Builtins short-circuit with no filesystem touch (the
/// resolver itself does this); files are read once with `std::fs::read_to_string` (tenet 4) and
/// lowered by [`source_to_js`]. The returned [`LoadAction`] is everything the loader needs to
/// evaluate and cache the module.
///
/// `referrer` is the absolute path of the importing module, or `None` for the entry module / an
/// `eval` context (resolution then falls back to the runtime CWD).
pub(crate) fn resolve_and_load(
    ctx: &NodeCtx,
    referrer: Option<&Path>,
    specifier: &str,
) -> Result<LoadAction, InstallError> {
    // Fast path: a `node:` builtin never touches the filesystem. We re-derive the static specifier
    // and fn pointer from the table so the action borrows static memory (zero allocation).
    if let Some(install) = lookup(specifier) {
        let bare = specifier.strip_prefix("node:").unwrap_or(specifier);
        if let Some((static_spec, _)) = crate::node::BUILTINS.iter().find(|(s, _)| *s == bare) {
            return Ok(LoadAction::Builtin { specifier: static_spec, install });
        }
    }

    let base = base_dir_for(referrer, ctx.cwd());
    match ctx
        .resolver()
        .resolve(base, specifier)
        .map_err(|e| InstallError::Resolve(e.to_string()))?
    {
        // Defensive: the resolver may also classify a builtin (it shares `lookup`). Re-attach the
        // install fn so the loader has a uniform action.
        Resolved::Builtin(static_spec) => {
            let install = lookup(static_spec)
                .ok_or_else(|| InstallError::Resolve(format!("unknown builtin '{static_spec}'")))?;
            Ok(LoadAction::Builtin { specifier: static_spec, install })
        }
        Resolved::File(path, mut kind) => {
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| InstallError::Io(format!("read {}: {e}", path.display())))?;
            let source = source_to_js(&path, raw)?;
            // JSON resolves as a CommonJS value regardless of how the resolver classified it.
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                kind = ModuleKind::Json;
            }
            Ok(LoadAction::File { path: cache_key(&path), source, kind })
        }
    }
}

/// Uniform per-module entry. The resolver policy is an internal service, not an importable object;
/// this returns an empty object so the registry seam stays uniform with the leaf builtins.
pub(crate) fn install<'gc>(
    agent: &mut Agent,
    _ctx: &NodeCtx,
    gc: GcScope<'gc, '_>,
) -> Result<Object<'gc>, InstallError> {
    let obj = OrdinaryObject::create_empty_object(agent, gc.into_nogc());
    Ok(obj.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::core::{EnvMap, HostState};
    use std::path::Path;

    /// A `NodeCtx` rooted at a given cwd, for resolution policy tests that don't need an engine.
    fn ctx_at(cwd: &Path) -> HostState {
        HostState::new(cwd.to_path_buf(), EnvMap::new())
    }

    #[test]
    fn base_dir_uses_referrer_parent_else_cwd() {
        let cwd = Path::new("/project/root");
        let referrer = Path::new("/project/src/a.js");
        assert_eq!(base_dir_for(Some(referrer), cwd), Path::new("/project/src"));
        assert_eq!(base_dir_for(None, cwd), cwd);
        // A referrer with no parent (rare) also falls back to cwd.
        assert_eq!(base_dir_for(Some(Path::new("a.js")), cwd), Path::new(""));
    }

    #[test]
    fn extension_classifiers() {
        assert!(is_typescript(Some("ts")));
        assert!(is_typescript(Some("mts")));
        assert!(is_typescript(Some("cts")));
        assert!(!is_typescript(Some("js")));
        assert!(!is_typescript(None));
        assert!(is_json(Some("json")));
        assert!(!is_json(Some("js")));
    }

    #[test]
    fn js_source_passes_through_unchanged() {
        let p = Path::new("mod.js");
        let js = source_to_js(p, "const x = 1; module.exports = x;".to_owned()).unwrap();
        assert_eq!(js, "const x = 1; module.exports = x;");
    }

    #[test]
    fn ts_source_is_transpiled_to_js() {
        let p = Path::new("mod.ts");
        let js = source_to_js(p, "const x: number = 1; export const y: string = 'a';".to_owned())
            .unwrap();
        assert!(!js.contains(": number"), "TS types should be stripped: {js}");
        assert!(!js.contains(": string"), "TS types should be stripped: {js}");
        assert!(js.contains("const x"));
    }

    #[test]
    fn json_source_is_wrapped_for_commonjs() {
        let p = Path::new("data.json");
        let js = source_to_js(p, "{\"a\":1,\"s\":\"hi\"}".to_owned()).unwrap();
        assert!(js.starts_with("module.exports = JSON.parse("), "got: {js}");
        assert!(js.contains("\\\"a\\\""), "keys should be escaped inside the literal: {js}");
        assert!(js.ends_with(");"));
    }

    #[test]
    fn js_string_literal_escapes_control_and_quote_chars() {
        let lit = encode_js_string_literal("a\"b\\c\nd\te\rf\u{2028}g\u{0001}h");
        assert!(lit.starts_with('"') && lit.ends_with('"'));
        assert!(lit.contains("\\\""));
        assert!(lit.contains("\\\\"));
        assert!(lit.contains("\\n"));
        assert!(lit.contains("\\t"));
        assert!(lit.contains("\\r"));
        assert!(lit.contains("\\u2028"));
        assert!(lit.contains("\\u0001"));
    }

    #[test]
    fn resolve_and_load_short_circuits_builtins_without_fs() {
        let state = ctx_at(Path::new("/nonexistent/treaty/dir"));
        let ctx = NodeCtx::new(&state);
        let action = resolve_and_load(&ctx, None, "node:path").unwrap();
        match action {
            LoadAction::Builtin { specifier, .. } => assert_eq!(specifier, "path"),
            other => panic!("expected builtin, got {other:?}"),
        }
        // Bare form too.
        match resolve_and_load(&ctx, None, "fs/promises").unwrap() {
            LoadAction::Builtin { specifier, .. } => assert_eq!(specifier, "fs/promises"),
            other => panic!("expected builtin, got {other:?}"),
        }
    }

    #[test]
    fn resolve_and_load_reads_and_lowers_a_real_ts_file() {
        let dir = std::env::temp_dir().join(format!("treaty-modres-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("greet.ts");
        std::fs::write(&file, "export const greeting: string = 'hi';\n").unwrap();

        let state = ctx_at(&dir);
        let ctx = NodeCtx::new(&state);
        // Resolve relative to the entry (cwd == dir): "./greet" must find greet.ts and transpile it.
        let action = resolve_and_load(&ctx, None, "./greet").unwrap();
        match action {
            LoadAction::File { path, source, .. } => {
                assert!(path.ends_with("greet.ts"), "cache key should be the .ts file: {path:?}");
                assert!(!source.contains(": string"), "types should be stripped: {source}");
                assert!(source.contains("greeting"));
            }
            other => panic!("expected file, got {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_and_load_reports_missing_file_as_resolve_error() {
        let dir = std::env::temp_dir();
        let state = ctx_at(&dir);
        let ctx = NodeCtx::new(&state);
        let err = resolve_and_load(&ctx, None, "./definitely-not-here-xyz").unwrap_err();
        matches!(err, InstallError::Resolve(_));
    }
}
