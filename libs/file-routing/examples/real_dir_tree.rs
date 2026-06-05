//! End-to-end proof that [`treaty_file_routing`] works against a REAL directory
//! tree, not just the in-memory `MemTree` fixtures used by the unit tests.
//!
//! This wires a filesystem-backed [`DirTree`] (`FsTree`) over `std::fs` into the
//! public [`generate_routing`] pipeline and prints the resulting
//! [`GeneratedRouting`] as pretty JSON. Point it at the bundled example app:
//!
//! ```text
//! cargo run --manifest-path libs/file-routing/Cargo.toml \
//!   --example real_dir_tree -- examples/file-routed-app
//! ```
//!
//! The path argument is the project root that *contains* the `routes/` and
//! `api/` directories. It defaults to `examples/file-routed-app` (relative to
//! the repo root) so the example is runnable with no arguments from there.

use std::fs;
use std::path::{Path, PathBuf};

use treaty_file_routing::{
    generate_routing, DirTree, Entry, FileRoutingConfig, GeneratedRouting,
};

/// A real-filesystem [`DirTree`]. It is rooted at an absolute base directory and
/// maps the tree-relative, `/`-separated paths the scanner asks for onto real
/// `std::fs` directory listings. A missing or non-directory path yields `[]`,
/// matching the trait contract, so the scanner degrades gracefully.
struct FsTree {
    base: PathBuf,
}

impl FsTree {
    fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    /// Resolve a tree-relative `/`-separated path against the base directory.
    fn resolve(&self, rel: &str) -> PathBuf {
        if rel.is_empty() {
            return self.base.clone();
        }
        let mut p = self.base.clone();
        for part in rel.split('/') {
            p.push(part);
        }
        p
    }
}

impl DirTree for FsTree {
    fn entries(&self, path: &str) -> Vec<Entry> {
        let dir = self.resolve(path);
        let Ok(read) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        read.filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_string();
                let kind = entry.file_type().ok()?;
                Some(if kind.is_dir() {
                    Entry::dir(name)
                } else {
                    Entry::file(name)
                })
            })
            .collect()
    }
}

fn main() {
    let arg = std::env::args().nth(1);
    let base = arg
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("examples/file-routed-app").to_path_buf());

    let config = FileRoutingConfig::default();
    let tree = FsTree::new(&base);
    let out: GeneratedRouting = generate_routing(&config, &tree);

    println!("# generate_routing against real dir: {}", base.display());
    println!("{}", serde_json::to_string_pretty(&out).unwrap());

    // A compact, assertion-friendly summary for the README / E2E test table.
    println!("\n## routes (path => component | layout)");
    print_routes(&out, &out.routes, 0);
    println!("\n## remotes (name => route_path => entry_file)");
    for r in &out.remotes {
        println!("{:<14} {:<18} {}", r.name, route_disp(&r.route_path), r.entry_file);
    }
    println!("\n## endpoints (path => handler_file => params)");
    for e in &out.endpoints {
        println!("{:<16} {:<28} {:?}", e.path, e.handler_file, e.param_names);
    }
}

fn route_disp(p: &str) -> String {
    if p.is_empty() {
        "\"\"".to_string()
    } else {
        p.to_string()
    }
}

fn print_routes(_out: &GeneratedRouting, routes: &[treaty_file_routing::AngularRoute], depth: usize) {
    for r in routes {
        let indent = "  ".repeat(depth);
        let comp = r
            .component_file
            .as_deref()
            .or(r.layout_file.as_deref())
            .unwrap_or("(structural)");
        let kind = if r.layout_file.is_some() { "layout" } else { "leaf" };
        println!(
            "{indent}{:<16} [{kind}] {comp}",
            route_disp(&r.path)
        );
        print_routes(_out, &r.children, depth + 1);
    }
}
