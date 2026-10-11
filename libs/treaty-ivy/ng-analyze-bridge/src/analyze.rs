//! Runs ng-analyze over ONE in-memory source (plus optional resource files) and returns its raw
//! per-class metadata. Compiled only with the `analyze` feature.
//!
//! Everything is served from ng-analyze's overlay file system: the source, the resources and a
//! synthetic `tsconfig.json` all live under [`VIRTUAL_ROOT`]. Nothing is read from disk, so the
//! bridge sees exactly the bytes Treaty's bundler hands it (the parity guarantee depends on this).

use std::collections::HashMap;

use ng_analyze::{AnalysisResult, Analyzer, AnalyzerOptions};

/// Root of the virtual project. The source lives at `VIRTUAL_ROOT/<file_name>`.
pub const VIRTUAL_ROOT: &str = "/treaty-virtual";

/// Virtual absolute path of a file given its name relative to the project root.
pub fn virtual_path(relative: &str) -> String {
    let rel = relative.trim_start_matches("./").trim_start_matches('/');
    format!("{VIRTUAL_ROOT}/{rel}")
}

/// Analyses `source` as `file_name` (relative to the virtual root). `resources` are extra virtual
/// files (template and stylesheet contents, keyed by their path relative to the root).
pub(crate) fn analyze_source(
    file_name: &str,
    source: &str,
    resources: &[(String, String)],
) -> Result<AnalysisResult, String> {
    let source_path = virtual_path(file_name);
    let tsconfig_path = format!("{VIRTUAL_ROOT}/tsconfig.json");
    let tsconfig = format!(
        r#"{{"compilerOptions":{{"strict":true,"target":"ES2022"}},"files":["{source_path}"]}}"#
    );
    let mut virtual_files: HashMap<String, String> = HashMap::new();
    virtual_files.insert(tsconfig_path.clone(), tsconfig);
    virtual_files.insert(source_path.clone(), source.to_string());
    for (path, content) in resources {
        virtual_files.insert(virtual_path(path), content.clone());
    }
    let analyzer = Analyzer::new(AnalyzerOptions {
        tsconfig_path,
        virtual_files: Some(virtual_files),
        ..Default::default()
    })?;
    analyzer
        .get_metadata_for_file(source_path.clone())
        .ok_or_else(|| format!("ng-analyze returned no metadata for {source_path}"))
}
