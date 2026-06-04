//! Generate a real SSR-host fixture crate on disk so it can be `cargo check`ed
//! against the actual `treaty_ssr` / `axum` / `tower_http` crates — the strongest
//! proof that `emit_ssr_host` emits a host that not only PARSES but TYPE-CHECKS.
//!
//! Usage: `cargo run -p rust_authoring --example emit_ssr_host_fixture -- <out_dir>`
//! Writes `<out_dir>/src/{main.rs,treaty_server.rs,ssr_routes.rs}` and a
//! `Cargo.toml`. The harness then runs `cargo check --manifest-path
//! <out_dir>/Cargo.toml`.

use std::fs;
use std::path::PathBuf;

use rust_authoring::plugin::{
    emit_production_host, emit_ssr_host_defaults, AxumBackendPlugin, BackendPlugin,
};

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).expect("usage: <out_dir>"));
    let src = out.join("src");
    fs::create_dir_all(&src).expect("create src dir");

    // (1) The generated SSR host main.rs — the thing under check.
    fs::write(src.join("main.rs"), emit_ssr_host_defaults()).expect("write main.rs");

    // (2) A real generated server-fn module (the sibling `treaty_server.rs` the host declares),
    // produced by the SAME axum backend the production host links — its `build_router()` is what the
    // SSR host merges. We synthesize one server fn so the module is non-trivial.
    let server_fns = extract_one_fn();
    let server_module = AxumBackendPlugin.emit(&server_fns).server_module;
    fs::write(src.join("treaty_server.rs"), server_module).expect("write treaty_server.rs");

    // (3) The emitted route table module (`ssr_routes.rs`): a single route with a real dist/server Ivy
    // template + a render-time macro, exactly the shape the SSR build emits.
    fs::write(src.join("ssr_routes.rs"), routes_module()).expect("write ssr_routes.rs");

    // (4) The fixture Cargo.toml pulling the real runtime deps. `treaty_ssr` is referenced by an
    // absolute path so the check links the ACTUAL crate, not a stub.
    let ssr_path = workspace_root().join("libs").join("treaty-ssr");
    fs::write(out.join("Cargo.toml"), cargo_toml(&ssr_path)).expect("write Cargo.toml");

    // Also emit the production host so the harness can confirm both emitters parse, but the SSR host
    // is the one wired into the fixture's `main.rs`.
    let _ = emit_production_host("treaty_server", 3000);

    println!("wrote SSR host fixture to {}", out.display());
}

/// Extract a single TS server fn so the generated `treaty_server` module has a real handler + router.
fn extract_one_fn() -> Vec<rust_authoring::plugin::ServerFn> {
    let source = "server:ts {\n  function add(a: number, b: number): number { return a + b; }\n}\n";
    rust_authoring::plugin::extract_server_block(source).server_fns
}

/// A minimal `ssr_routes.rs`: `pub fn routes() -> SsrRouteTable` with one parameterized route whose
/// `dist/server` Ivy binds `ctx.title`, plus a render-time macro that reads the request.
fn routes_module() -> String {
    r##"//! Generated SSR route table (the dist/server Ivy per route). Emitted by the SSR build.
use treaty_ssr::{SsrRoute, SsrRouteTable};

pub fn routes() -> SsrRouteTable {
    SsrRouteTable::new(vec![SsrRoute {
        pattern: "/blog/:slug".to_string(),
        ivy_code: r#"
            function App_Template(rf, ctx) {
                if (rf & 1) {
                    i0.ɵɵelementStart(0, "h1");
                    i0.ɵɵtext(1);
                    i0.ɵɵelementEnd();
                }
                if (rf & 2) {
                    i0.ɵɵadvance(1);
                    i0.ɵɵtextInterpolate(ctx.title);
                }
            }
        "#
        .to_string(),
        component_id: "blog.tsx".to_string(),
        macro_source: Some("export default { title: 'Post: ' + input.params.slug }".to_string()),
        server_fns: Vec::new(),
    }])
}
"##
    .to_string()
}

/// The fixture crate manifest: a binary linking the real `treaty_ssr` plus axum/tokio/tower-http.
fn cargo_toml(ssr_path: &std::path::Path) -> String {
    let ssr = ssr_path.display().to_string().replace('\\', "/");
    format!(
        r#"[workspace]

[package]
name = "ssr_host_fixture"
version = "0.0.0"
edition = "2021"

[[bin]]
name = "ssr_host_fixture"
path = "src/main.rs"

[dependencies]
treaty_ssr = {{ path = "{ssr}" }}
axum = {{ version = "0.7", features = ["ws"] }}
tokio = {{ version = "1", features = ["rt-multi-thread", "macros", "net"] }}
tower-http = {{ version = "0.6", features = ["fs"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
"#
    )
}

/// Resolve the workspace root from this example's compile-time manifest dir
/// (`libs/authoring/rust`), walking up to the repo root.
fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // libs/authoring/rust -> up three -> repo root.
    manifest
        .ancestors()
        .nth(3)
        .map(PathBuf::from)
        .unwrap_or(manifest)
}
