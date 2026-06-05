//! Per-request server-function execution: the request-time data-loading step
//! that runs a route's server functions (their TypeScript bodies) on the server
//! via the Nova runtime, before the template is rendered.
//!
//! A request render is "load server data, then interpret the template against
//! it". The data-loading half reuses [`treaty_runtime::run_server_fn`] — the
//! exact Nova path the production axum host runs a `POST /__server/<name>` body
//! through — so a server fn evaluated to seed a render and one called over HTTP
//! execute identically. The results are merged into the render input under
//! `server.<name>` so the route's render-time macro reads them as
//! `input.server.load()`-style data.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One server function a route can run at request time to load data for its
/// render. The body is the function's TypeScript source (the same text the axum
/// backend lifts into a `/__server/<name>` handler); `args` is the JSON argument
/// array the body reads from its `args` global.
///
/// Kept as plain data so a host can build it from the build's server-fn manifest
/// and a test can write one literally. The execution is the injected
/// [`ServerFnExecutor`] (Nova in production, a fake in tests), so this crate
/// stays free of a hard Nova dependency at the type level.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SsrServerFn {
    /// The fn name; its result is exposed to the render-time macro as
    /// `input.server.<name>`.
    pub name: String,
    /// The TypeScript body of the server function (executed server-side).
    #[serde(default)]
    pub source: String,
    /// The JSON arguments passed to the body (read from its `args`/`__args`
    /// globals). A request-derived loader typically passes the route params here.
    #[serde(default)]
    pub args: Value,
}

/// The execute-one-server-fn seam — the Nova `run_server_fn` boundary, abstracted
/// so the handler depends on a SHAPE rather than embedding the runtime directly.
///
/// The production implementation ([`NovaServerFnExecutor`]) runs the body through
/// [`treaty_runtime::run_server_fn`]; tests inject a deterministic fake. A fn that
/// fails to execute yields `None` (the render proceeds without that datum) rather
/// than aborting the whole request — a single broken loader must not 500 the page.
pub trait ServerFnExecutor {
    /// Execute one server fn's body against its args, returning its JSON result
    /// or `None` if it threw / could not run.
    fn execute(&self, server_fn: &SsrServerFn) -> Option<Value>;
}

/// The production [`ServerFnExecutor`]: runs each fn body through the Nova
/// runtime, exactly as the production axum host runs a `/__server/<name>` call.
///
/// This is the single place the SSR path touches `treaty_runtime`, so the
/// request-time data load and the over-HTTP server-fn call share one engine and
/// one TS->JS step. A thrown body / transpile failure degrades to `None`.
#[derive(Debug, Default, Clone, Copy)]
pub struct NovaServerFnExecutor;

impl ServerFnExecutor for NovaServerFnExecutor {
    fn execute(&self, server_fn: &SsrServerFn) -> Option<Value> {
        // Reuse the runtime's server-fn path: transpile the TS body, run it in a
        // fresh Nova isolate with `args` injected, and capture the JSON result.
        // A failure (parse / runtime / conversion) is swallowed to `None` so one
        // broken loader does not abort the request render.
        treaty_runtime::run_server_fn(&server_fn.source, &server_fn.args).ok()
    }
}

/// Run every server fn for a route and collect their results into a JSON object
/// keyed by fn name. The object is merged into the render input under `server`,
/// so a render-time macro reads a loader result as `input.server.<name>`. Fns
/// that fail are omitted (their key is simply absent).
pub fn load_server_data(
    server_fns: &[SsrServerFn],
    executor: &dyn ServerFnExecutor,
) -> Value {
    let mut obj = serde_json::Map::new();
    for f in server_fns {
        if let Some(value) = executor.execute(f) {
            obj.insert(f.name.clone(), value);
        }
    }
    Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fake executor that returns a fixed value for a named fn and `None` for a
    /// fn whose name starts with `broken`.
    struct FakeExecutor;

    impl ServerFnExecutor for FakeExecutor {
        fn execute(&self, f: &SsrServerFn) -> Option<Value> {
            if f.name.starts_with("broken") {
                None
            } else {
                Some(json!({ "ran": f.name, "args": f.args }))
            }
        }
    }

    #[test]
    fn load_server_data_keys_results_by_name() {
        let fns = vec![
            SsrServerFn { name: "user".to_string(), source: String::new(), args: json!([1]) },
            SsrServerFn { name: "posts".to_string(), source: String::new(), args: json!([]) },
        ];
        let data = load_server_data(&fns, &FakeExecutor);
        assert_eq!(data["user"]["ran"], json!("user"));
        assert_eq!(data["user"]["args"], json!([1]));
        assert_eq!(data["posts"]["ran"], json!("posts"));
    }

    #[test]
    fn a_failing_fn_is_omitted_not_fatal() {
        let fns = vec![
            SsrServerFn { name: "ok".to_string(), ..Default::default() },
            SsrServerFn { name: "broken_loader".to_string(), ..Default::default() },
        ];
        let data = load_server_data(&fns, &FakeExecutor);
        assert!(data.get("ok").is_some());
        // The broken fn produced no key, and the render still got the ok datum.
        assert!(data.get("broken_loader").is_none());
    }

    #[test]
    fn nova_executor_runs_a_real_ts_body() {
        // Exercises the REAL Nova path (no fake): a TS server fn that reads its
        // args and returns a value must execute and round-trip through JSON.
        let f = SsrServerFn {
            name: "sum".to_string(),
            source: "return args[0] + args[1];".to_string(),
            args: json!([2, 3]),
        };
        let out = NovaServerFnExecutor.execute(&f);
        assert_eq!(out, Some(json!(5)));
    }
}
