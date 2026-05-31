//! Stage 2 (api): lower a scanned [`ApiNode`] tree into a flat server-endpoint
//! manifest for the server-fn / backend-plugin model.
//!
//! Treaty is a compiler, not a host: this module emits *data* describing the
//! endpoints — their HTTP methods, URL path, and a stable handler reference —
//! and a backend plugin (axum by default, Elysia opt-in) consumes the manifest
//! to actually mount the handlers. Endpoint *bodies* run server-side; this code
//! never invents a runtime.
//!
//! Each [`ApiHandlerFile`] in the scanned tree becomes one [`ServerEndpoint`]
//! whose:
//! - **path** is the join of its ancestor directory segments plus the file
//!   segment, with dynamic segments emitted as `:param` and the ordered
//!   parameter names collected (catch-all `[...rest]` / `[..rest]` / `*` becomes
//!   a trailing `*rest` segment);
//! - **methods** are inferred from the file's base name (e.g. `get`, `post`,
//!   `users.post`, `route.delete`) and otherwise default to a configurable set;
//! - **handler_ref** is a stable, slugified identifier the backend plugin keys
//!   the bundled handler module by.
//!
//! The public [`build_endpoints`] entry point keeps the crate-wide
//! [`ApiEndpoint`] shape that [`crate::generate_routing`] threads through, while
//! [`build_manifest`] exposes the richer method/handler-ref view for backend
//! plugins. Both walk the same tree and are sorted for determinism.

use crate::config::{ApiEndpoint, ApiNode, FileRoutingConfig};
use serde::{Deserialize, Serialize};

/// HTTP methods an endpoint can answer. Kept as a small closed enum so the
/// manifest is self-describing and a backend plugin can match exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Head,
}

impl HttpMethod {
    /// All methods, in canonical order. Used as the inferred default when a
    /// handler's name carries no method hint.
    pub const ALL: [HttpMethod; 7] = [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Options,
        HttpMethod::Head,
    ];

    /// Parse a method hint (case-insensitive) from a name fragment. Returns
    /// `None` when the fragment is not a recognised HTTP method.
    pub fn from_hint(s: &str) -> Option<HttpMethod> {
        match s.to_ascii_lowercase().as_str() {
            "get" => Some(HttpMethod::Get),
            "post" => Some(HttpMethod::Post),
            "put" => Some(HttpMethod::Put),
            "patch" => Some(HttpMethod::Patch),
            "delete" | "del" => Some(HttpMethod::Delete),
            "options" => Some(HttpMethod::Options),
            "head" => Some(HttpMethod::Head),
            _ => None,
        }
    }

    /// Canonical uppercase wire name (e.g. `"GET"`).
    pub fn as_str(self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Options => "OPTIONS",
            HttpMethod::Head => "HEAD",
        }
    }
}

/// A fully-resolved server endpoint for the backend-plugin manifest.
///
/// This is the richer view the server-fn model needs: the methods it answers,
/// its URL path (dynamic segments as `:param`, catch-all as `*param`), the
/// ordered parameter names, the handler file, and a stable `handler_ref` the
/// backend keys the compiled handler by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerEndpoint {
    /// HTTP methods this endpoint answers, sorted and de-duplicated.
    pub methods: Vec<HttpMethod>,
    /// URL path, leading slash, dynamic segments as `:param`, catch-all `*param`.
    pub path: String,
    /// Tree-relative path of the handler file.
    pub handler_file: String,
    /// Stable, slugified handler identifier the backend plugin keys on.
    pub handler_ref: String,
    /// Ordered names of dynamic parameters appearing in `path`.
    pub param_names: Vec<String>,
    /// `true` when the trailing segment is a catch-all (`*param`).
    pub is_catch_all: bool,
}

/// Lower the scanned api tree into the crate-wide [`ApiEndpoint`] list that
/// [`crate::generate_routing`] threads through, sorted by path for determinism.
/// An absent root (`None`) yields an empty `Vec`.
///
/// This is the path/param view; for the method + handler-ref view a backend
/// plugin consumes, see [`build_manifest`].
pub fn build_endpoints(config: &FileRoutingConfig, root: Option<&ApiNode>) -> Vec<ApiEndpoint> {
    build_manifest(config, root)
        .into_iter()
        .map(|e| ApiEndpoint {
            path: e.path,
            handler_file: e.handler_file,
            param_names: e.param_names,
        })
        .collect()
}

/// Lower the scanned api tree into the full [`ServerEndpoint`] manifest for the
/// backend plugin: method inference, dynamic/catch-all params, and a stable
/// handler reference per endpoint. Sorted by `(path, methods)` for determinism.
/// An absent root (`None`) yields an empty `Vec`.
pub fn build_manifest(config: &FileRoutingConfig, root: Option<&ApiNode>) -> Vec<ServerEndpoint> {
    let mut out = Vec::new();
    if let Some(node) = root {
        // The root node contributes no leading segment; ancestor segments start
        // empty and accumulate as we descend.
        walk(config, node, &[], &mut out);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.methods.cmp(&b.methods)));
    out
}

/// Recursively visit `node`, emitting one [`ServerEndpoint`] per handler file.
/// `ancestor_segments` are the already-normalised URL segments of the path from
/// the api root down to (but excluding) `node`'s own segment.
fn walk(
    config: &FileRoutingConfig,
    node: &ApiNode,
    ancestor_segments: &[String],
    out: &mut Vec<ServerEndpoint>,
) {
    // This node's own segment joins the ancestors for everything beneath it.
    let mut dir_segments: Vec<String> = ancestor_segments.to_vec();
    if !node.segment.is_empty() {
        dir_segments.push(normalize_segment(&node.segment, node.is_dynamic));
    }

    for handler in &node.handler_files {
        // An index handler contributes no extra segment; a leaf handler appends
        // its own (possibly dynamic / catch-all) segment.
        let mut segments = dir_segments.clone();
        let is_catch_all = is_catch_all_segment(&handler.segment);
        if !handler.is_index && !handler.segment.is_empty() {
            segments.push(normalize_segment(&handler.segment, handler.is_dynamic));
        }

        let path = join_path(&segments);
        let param_names = collect_params(&segments);
        let methods = infer_methods(config, handler.file_path.as_str());
        let handler_ref = handler_ref_for(&segments, &methods);

        out.push(ServerEndpoint {
            methods,
            path,
            handler_file: handler.file_path.clone(),
            handler_ref,
            param_names,
            is_catch_all,
        });
    }

    for child in &node.children {
        walk(config, child, &dir_segments, out);
    }
}

/// Normalise a scanner-emitted segment into its URL form. Dynamic segments are
/// rendered `:param`; catch-all dynamic segments `*param`; static segments pass
/// through unchanged. The scanner may already emit `:param`/`*param`, so this is
/// idempotent and also tolerant of a raw `[param]` / `[...rest]` leaking through.
fn normalize_segment(segment: &str, is_dynamic: bool) -> String {
    if let Some(rest) = catch_all_param(segment) {
        return format!("*{rest}");
    }
    if let Some(param) = bracket_param(segment) {
        return format!(":{param}");
    }
    if is_dynamic && !segment.starts_with(':') && !segment.starts_with('*') {
        return format!(":{segment}");
    }
    segment.to_string()
}

/// Whether a (possibly already-normalised) segment is a catch-all.
fn is_catch_all_segment(segment: &str) -> bool {
    segment.starts_with('*') || catch_all_param(segment).is_some()
}

/// Extract the parameter name from a catch-all spelling: `[...rest]`, `[..rest]`,
/// or a bare `*` / `*rest`. Returns the bare name (`"rest"`, or `"rest"` for a
/// lone `*`).
fn catch_all_param(segment: &str) -> Option<String> {
    if let Some(inner) = bracket_inner(segment) {
        let trimmed = inner.trim_start_matches('.');
        if inner.len() != trimmed.len() {
            let name = if trimmed.is_empty() { "rest" } else { trimmed };
            return Some(name.to_string());
        }
        return None;
    }
    if let Some(rest) = segment.strip_prefix('*') {
        let name = if rest.is_empty() { "rest" } else { rest };
        return Some(name.to_string());
    }
    None
}

/// Extract the parameter name from a non-catch-all bracket segment `[param]`.
fn bracket_param(segment: &str) -> Option<String> {
    bracket_inner(segment).and_then(|inner| {
        // A leading `.` marks a catch-all (handled separately); empty brackets
        // are not a parameter.
        if inner.starts_with('.') || inner.is_empty() {
            None
        } else {
            Some(inner.to_string())
        }
    })
}

/// The text inside a `[...]` segment, if `segment` is bracketed.
fn bracket_inner(segment: &str) -> Option<&str> {
    segment
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
}

/// Join URL `segments` into a leading-slash path. No segments → `"/"`.
fn join_path(segments: &[String]) -> String {
    if segments.is_empty() {
        return "/".to_string();
    }
    let mut path = String::new();
    for seg in segments {
        path.push('/');
        path.push_str(seg);
    }
    path
}

/// Collect ordered parameter names from normalised `segments` (`:p` and `*p`).
fn collect_params(segments: &[String]) -> Vec<String> {
    segments
        .iter()
        .filter_map(|s| {
            s.strip_prefix(':')
                .or_else(|| s.strip_prefix('*'))
                .map(str::to_string)
        })
        .collect()
}

/// Infer the HTTP methods a handler answers from its file base name.
///
/// The base name (extension stripped via the configured api extensions, else
/// the raw stem) is split on `.` and `-`; any fragment that names a method
/// contributes that method. Recognised: `get`, `post`, `put`, `patch`,
/// `delete`/`del`, `options`, `head` (case-insensitive). When no fragment names
/// a method, the endpoint answers all methods (`HttpMethod::ALL`) — a single
/// catch-all handler in the server-fn model, which the backend plugin narrows.
fn infer_methods(config: &FileRoutingConfig, file_path: &str) -> Vec<HttpMethod> {
    let file_name = file_path.rsplit('/').next().unwrap_or(file_path);
    let stem = config
        .match_api_extension(file_name)
        .map(|(stem, _ext)| stem)
        .unwrap_or(file_name);

    let mut methods: Vec<HttpMethod> = stem
        .split(['.', '-'])
        .filter_map(HttpMethod::from_hint)
        .collect();

    if methods.is_empty() {
        return HttpMethod::ALL.to_vec();
    }

    methods.sort();
    methods.dedup();
    methods
}

/// Build a stable, slugified handler reference from the path `segments` and the
/// inferred `methods`. Deterministic and collision-resistant across method
/// variants of the same path (e.g. `users.get` vs `users.post`).
///
/// Form: `<seg>_<seg>__<method>[_<method>...]`, dynamic markers folded into the
/// segment name (`:id` → `id`, `*rest` → `rest`). The api root index becomes
/// `root`.
fn handler_ref_for(segments: &[String], methods: &[HttpMethod]) -> String {
    let body = if segments.is_empty() {
        "root".to_string()
    } else {
        segments
            .iter()
            .map(|s| slug(s))
            .collect::<Vec<_>>()
            .join("_")
    };
    let method_part = methods
        .iter()
        .map(|m| m.as_str().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("_");
    format!("{body}__{method_part}")
}

/// Slugify one URL segment for use in an identifier: strip dynamic markers and
/// replace any non-alphanumeric run with a single `_`.
fn slug(segment: &str) -> String {
    let bare = segment
        .trim_start_matches(':')
        .trim_start_matches('*');
    let mut out = String::with_capacity(bare.len());
    let mut prev_us = false;
    for ch in bare.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "seg".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ApiHandlerFile;

    fn handler(file_path: &str, segment: &str, is_index: bool, is_dynamic: bool) -> ApiHandlerFile {
        let param_name = if is_dynamic {
            Some(
                segment
                    .trim_start_matches(':')
                    .trim_start_matches('*')
                    .to_string(),
            )
        } else {
            None
        };
        ApiHandlerFile {
            file_path: file_path.to_string(),
            segment: segment.to_string(),
            is_index,
            is_dynamic,
            param_name,
        }
    }

    fn node(
        dir_path: &str,
        segment: &str,
        is_dynamic: bool,
        handler_files: Vec<ApiHandlerFile>,
        children: Vec<ApiNode>,
    ) -> ApiNode {
        let param_name = if is_dynamic {
            Some(
                segment
                    .trim_start_matches(':')
                    .trim_start_matches('*')
                    .to_string(),
            )
        } else {
            None
        };
        ApiNode {
            dir_path: dir_path.to_string(),
            segment: segment.to_string(),
            is_dynamic,
            param_name,
            handler_files,
            children,
        }
    }

    fn cfg() -> FileRoutingConfig {
        FileRoutingConfig::default()
    }

    /// Find the single endpoint whose path equals `path`. Panics if not exactly one.
    fn at<'a>(eps: &'a [ServerEndpoint], path: &str) -> &'a ServerEndpoint {
        let matches: Vec<&ServerEndpoint> = eps.iter().filter(|e| e.path == path).collect();
        assert_eq!(matches.len(), 1, "expected exactly one endpoint at {path}, got {matches:?}");
        matches[0]
    }

    #[test]
    fn none_root_yields_empty() {
        assert!(build_manifest(&cfg(), None).is_empty());
        assert!(build_endpoints(&cfg(), None).is_empty());
    }

    #[test]
    fn root_index_maps_to_slash() {
        let root = node(
            "api",
            "",
            false,
            vec![handler("api/index.ts", "", true, false)],
            vec![],
        );
        let eps = build_manifest(&cfg(), Some(&root));
        assert_eq!(eps.len(), 1);
        let ep = &eps[0];
        assert_eq!(ep.path, "/");
        assert_eq!(ep.methods, HttpMethod::ALL.to_vec());
        assert_eq!(ep.handler_file, "api/index.ts");
        assert_eq!(ep.handler_ref, "root__get_post_put_patch_delete_options_head");
        assert!(ep.param_names.is_empty());
        assert!(!ep.is_catch_all);
    }

    #[test]
    fn nested_static_path_joins_dir_segments() {
        // api/users/profile.get.ts -> GET /users/profile
        let users = node(
            "api/users",
            "users",
            false,
            vec![handler("api/users/profile.get.ts", "profile", false, false)],
            vec![],
        );
        let root = node("api", "", false, vec![], vec![users]);
        let eps = build_manifest(&cfg(), Some(&root));
        let ep = at(&eps, "/users/profile");
        assert_eq!(ep.methods, vec![HttpMethod::Get]);
        assert_eq!(ep.handler_ref, "users_profile__get");
        assert!(ep.param_names.is_empty());
    }

    #[test]
    fn method_inferred_from_file_name() {
        // index handlers named after the verb: get.ts / post.ts in api/users
        let users = node(
            "api/users",
            "users",
            false,
            vec![
                handler("api/users/get.ts", "", true, false),
                handler("api/users/post.ts", "", true, false),
            ],
            vec![],
        );
        let root = node("api", "", false, vec![], vec![users]);
        let eps = build_manifest(&cfg(), Some(&root));
        // Both resolve to /users but with distinct methods + handler refs.
        let users_eps: Vec<&ServerEndpoint> = eps.iter().filter(|e| e.path == "/users").collect();
        assert_eq!(users_eps.len(), 2);
        let get = users_eps.iter().find(|e| e.methods == vec![HttpMethod::Get]).unwrap();
        let post = users_eps.iter().find(|e| e.methods == vec![HttpMethod::Post]).unwrap();
        assert_eq!(get.handler_ref, "users__get");
        assert_eq!(post.handler_ref, "users__post");
    }

    #[test]
    fn dotted_method_suffix_is_inferred() {
        // route.delete.ts -> DELETE, with "del" alias too
        let root = node(
            "api",
            "",
            false,
            vec![
                handler("api/route.delete.ts", "route", false, false),
                handler("api/thing.del.ts", "thing", false, false),
            ],
            vec![],
        );
        let eps = build_manifest(&cfg(), Some(&root));
        assert_eq!(at(&eps, "/route").methods, vec![HttpMethod::Delete]);
        assert_eq!(at(&eps, "/thing").methods, vec![HttpMethod::Delete]);
    }

    #[test]
    fn multiple_methods_in_name_dedup_and_sort() {
        // post.get.get.ts -> {GET, POST} sorted, deduped
        let root = node(
            "api",
            "",
            false,
            vec![handler("api/post.get.get.ts", "thing", false, false)],
            vec![],
        );
        let eps = build_manifest(&cfg(), Some(&root));
        let ep = at(&eps, "/thing");
        assert_eq!(ep.methods, vec![HttpMethod::Get, HttpMethod::Post]);
        assert_eq!(ep.handler_ref, "thing__get_post");
    }

    #[test]
    fn bracket_dynamic_param_in_dir_and_file() {
        // api/users/[id]/index.ts (dir dynamic) and api/posts/[slug].get.ts (file dynamic)
        let id_dir = node(
            "api/users/[id]",
            "[id]",
            true,
            vec![handler("api/users/[id]/index.ts", "", true, false)],
            vec![],
        );
        let users = node("api/users", "users", false, vec![], vec![id_dir]);
        let posts = node(
            "api/posts",
            "posts",
            false,
            vec![handler("api/posts/[slug].get.ts", "[slug]", false, true)],
            vec![],
        );
        let root = node("api", "", false, vec![], vec![users, posts]);
        let eps = build_manifest(&cfg(), Some(&root));

        let user = at(&eps, "/users/:id");
        assert_eq!(user.param_names, vec!["id".to_string()]);
        assert!(!user.is_catch_all);

        let post = at(&eps, "/posts/:slug");
        assert_eq!(post.methods, vec![HttpMethod::Get]);
        assert_eq!(post.param_names, vec!["slug".to_string()]);
        assert_eq!(post.handler_ref, "posts_slug__get");
    }

    #[test]
    fn colon_style_dynamic_segment_passes_through() {
        // scanner already emitting :id form
        let id_dir = node(
            "api/users/:id",
            ":id",
            true,
            vec![handler("api/users/:id/index.ts", "", true, false)],
            vec![],
        );
        let users = node("api/users", "users", false, vec![], vec![id_dir]);
        let root = node("api", "", false, vec![], vec![users]);
        let eps = build_manifest(&cfg(), Some(&root));
        let ep = at(&eps, "/users/:id");
        assert_eq!(ep.param_names, vec!["id".to_string()]);
    }

    #[test]
    fn catch_all_bracket_and_star_forms() {
        // api/files/[...path].ts and api/blob/[..rest].ts and api/raw/*.ts
        let files = node(
            "api/files",
            "files",
            false,
            vec![handler("api/files/[...path].ts", "[...path]", false, true)],
            vec![],
        );
        let blob = node(
            "api/blob",
            "blob",
            false,
            vec![handler("api/blob/[..rest].ts", "[..rest]", false, true)],
            vec![],
        );
        let raw = node(
            "api/raw",
            "raw",
            false,
            vec![handler("api/raw/all.ts", "*", false, true)],
            vec![],
        );
        let root = node("api", "", false, vec![], vec![files, blob, raw]);
        let eps = build_manifest(&cfg(), Some(&root));

        let f = at(&eps, "/files/*path");
        assert!(f.is_catch_all);
        assert_eq!(f.param_names, vec!["path".to_string()]);

        let b = at(&eps, "/blob/*rest");
        assert!(b.is_catch_all);
        assert_eq!(b.param_names, vec!["rest".to_string()]);

        let r = at(&eps, "/raw/*rest");
        assert!(r.is_catch_all);
        assert_eq!(r.param_names, vec!["rest".to_string()]);
    }

    #[test]
    fn multiple_dynamic_params_ordered() {
        // api/orgs/[org]/repos/[repo].get.ts -> /orgs/:org/repos/:repo
        let repo_file = node(
            "api/orgs/[org]/repos",
            "repos",
            false,
            vec![handler(
                "api/orgs/[org]/repos/[repo].get.ts",
                "[repo]",
                false,
                true,
            )],
            vec![],
        );
        let org = node("api/orgs/[org]", "[org]", true, vec![], vec![repo_file]);
        let orgs = node("api/orgs", "orgs", false, vec![], vec![org]);
        let root = node("api", "", false, vec![], vec![orgs]);
        let eps = build_manifest(&cfg(), Some(&root));
        let ep = at(&eps, "/orgs/:org/repos/:repo");
        assert_eq!(ep.param_names, vec!["org".to_string(), "repo".to_string()]);
        assert_eq!(ep.methods, vec![HttpMethod::Get]);
        assert_eq!(ep.handler_ref, "orgs_org_repos_repo__get");
    }

    #[test]
    fn build_endpoints_projects_path_view_and_sorts() {
        let users = node(
            "api/users",
            "users",
            false,
            vec![
                handler("api/users/post.ts", "", true, false),
                handler("api/users/get.ts", "", true, false),
            ],
            vec![],
        );
        let root = node(
            "api",
            "",
            false,
            vec![handler("api/health.get.ts", "health", false, false)],
            vec![users],
        );
        let eps = build_endpoints(&cfg(), Some(&root));
        // Three endpoints; sorted by path then methods.
        let paths: Vec<&str> = eps.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["/health", "/users", "/users"]);
        // The projected ApiEndpoint keeps path/handler_file/param_names only.
        assert_eq!(eps[0].handler_file, "api/health.get.ts");
        assert!(eps[0].param_names.is_empty());
    }

    #[test]
    fn manifest_serde_round_trips() {
        let root = node(
            "api",
            "",
            false,
            vec![handler("api/users/[id].get.ts", "[id]", false, true)],
            vec![],
        );
        let eps = build_manifest(&cfg(), Some(&root));
        let json = serde_json::to_string(&eps).unwrap();
        let back: Vec<ServerEndpoint> = serde_json::from_str(&json).unwrap();
        assert_eq!(eps, back);
        // Methods serialize as uppercase wire names.
        assert!(json.contains("\"GET\""));
    }

    #[test]
    fn custom_api_extension_strips_correctly() {
        // .treaty is a configured api extension; method still inferred from stem.
        let root = node(
            "api",
            "",
            false,
            vec![handler("api/ping.post.treaty", "ping", false, false)],
            vec![],
        );
        let eps = build_manifest(&cfg(), Some(&root));
        let ep = at(&eps, "/ping");
        assert_eq!(ep.methods, vec![HttpMethod::Post]);
    }
}
