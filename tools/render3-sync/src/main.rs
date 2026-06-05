//! `render3-sync` CLI — the entry point for the deterministic (NO-AI) render3 sync harness.
//!
//! Subcommands:
//!   * `map` — print the symbol->module map as JSON (the maintained pillar-1 table).
//!   * `exports <ts-file>` — parse a TS file under the vendored Angular ref and print its exported
//!     symbol names + the Rust modules each maps to.
//!   * `baseline [--repo-root <dir>] [--ref <label>] [--out <file>]` — (re)record the export
//!     fingerprint of every symbol-map reference source at the current `angular-ref` into a
//!     baseline artifact (default `tools/render3-sync/baseline.json`).
//!   * `drift [--repo-root <dir>] [--baseline <file>] [--ref <label>] [--out <file>]` — re-fingerprint
//!     the live references and diff against the recorded baseline, printing the [`DriftReport`]
//!     (Added/Removed/Modified exports + derived PortTasks) as JSON. Exits 1 on drift.
//!   * `codegen-verify [--repo-root <dir>] [--out <file>]` — for each Mechanical symbol-map row,
//!     emit Rust from the vendored TS and diff it against the committed Rust under
//!     `libs/treaty-ivy/*`, printing a [`VerifyReport`]. Exits 1 if any row is out of sync.
//!
//! These commands read the LIVE filesystem read-only via [`render3_sync::FsReader`]; they never
//! modify the `libs/treaty-ivy/*` ports. See `migration/RENDER3-SYNC-PLAN.md`.

use std::path::PathBuf;
use std::process::ExitCode;

use render3_sync::sync::{self, Baseline, FsReader};
use render3_sync::symbol_map::{self, MODULE_MAP};
use render3_sync::ts;

/// The default baseline artifact path, relative to the located repo root.
const DEFAULT_BASELINE: &str = "tools/render3-sync/baseline.json";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("map") => {
            print_map();
            ExitCode::SUCCESS
        }
        Some("exports") => cmd_exports(&args[1..]),
        Some("baseline") => cmd_baseline(&args[1..]),
        Some("drift") => cmd_drift(&args[1..]),
        Some("codegen-verify") => cmd_codegen_verify(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "render3-sync — keep the Rust render3 port 1:1 with Angular (NO AI)\n\
     usage:\n  \
     render3-sync map                                  print the symbol->module map as JSON\n  \
     render3-sync exports <ts-file>                    list exported symbols of a TS file + Rust owners\n  \
     render3-sync baseline [--repo-root D] [--ref R] [--out F]\n                                                   record the export-fingerprint baseline\n  \
     render3-sync drift    [--repo-root D] [--baseline F] [--ref R] [--out F]\n                                                   diff live refs vs baseline -> DriftReport (exit 1 on drift)\n  \
     render3-sync codegen-verify [--repo-root D] [--out F]\n                                                   verify mechanical tables vs committed Rust (exit 1 on drift)";

/// Serialize the static symbol->module map to JSON on stdout.
fn print_map() {
    match serde_json::to_string_pretty(MODULE_MAP) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("render3-sync: failed to serialize map: {e}"),
    }
}

fn cmd_exports(args: &[String]) -> ExitCode {
    let Some(path) = args.first() else {
        eprintln!("render3-sync: `exports` requires a path to a .ts file");
        return ExitCode::FAILURE;
    };
    match std::fs::read_to_string(path) {
        Ok(src) => {
            let exports = ts::exported_symbols(&src);
            println!("{path}: {} exported symbols", exports.len());
            for sym in &exports {
                let owners = symbol_map::rust_modules_for_symbol(&sym.name);
                let owners: Vec<&str> = owners.iter().map(|m| m.rust_file).collect();
                let owned =
                    if owners.is_empty() { "<unmapped>".to_string() } else { owners.join(", ") };
                println!("  {:<40} {:?}  ->  {owned}", sym.name, sym.kind);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("render3-sync: cannot read {path}: {e}");
            ExitCode::FAILURE
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Shared flag parsing (deterministic, unit-tested).
// ------------------------------------------------------------------------------------------------

/// Parsed common CLI options for the live-tree commands.
#[derive(Debug, Default, PartialEq, Eq)]
struct Opts {
    repo_root: Option<PathBuf>,
    baseline: Option<PathBuf>,
    out: Option<PathBuf>,
    angular_ref: Option<String>,
}

/// Parse `--repo-root`, `--baseline`, `--out`, `--ref` flags. Unknown flags are an error so a typo
/// never silently no-ops. Returns the parsed options or a message for the caller to print.
fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    let mut i = 0;
    while i < args.len() {
        let flag = args[i].as_str();
        let val = || args.get(i + 1).cloned().ok_or_else(|| format!("{flag} requires a value"));
        match flag {
            "--repo-root" => o.repo_root = Some(PathBuf::from(val()?)),
            "--baseline" => o.baseline = Some(PathBuf::from(val()?)),
            "--out" => o.out = Some(PathBuf::from(val()?)),
            "--ref" => o.angular_ref = Some(val()?),
            other => return Err(format!("unknown flag {other}")),
        }
        i += 2;
    }
    Ok(o)
}

/// Resolve the repo root: the explicit `--repo-root`, else the nearest ancestor of CWD that
/// contains both `tools/angular-ref` and `libs/treaty-ivy` (the relocated render3 port).
fn resolve_repo_root(explicit: Option<PathBuf>) -> Option<PathBuf> {
    if let Some(r) = explicit {
        return Some(r);
    }
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("tools/angular-ref").is_dir() && dir.join("libs/treaty-ivy").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

// ------------------------------------------------------------------------------------------------
// `baseline`
// ------------------------------------------------------------------------------------------------

fn cmd_baseline(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("render3-sync baseline: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(root) = resolve_repo_root(opts.repo_root.clone()) else {
        eprintln!("render3-sync baseline: could not locate repo root; pass --repo-root");
        return ExitCode::FAILURE;
    };
    let reader = FsReader::new(&root);
    let angular_ref = opts.angular_ref.as_deref().unwrap_or("current");

    let baseline = match sync::record_baseline(&reader, angular_ref) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("render3-sync baseline: reading references failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    let json = match serde_json::to_string_pretty(&baseline) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("render3-sync baseline: serialize failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let out = opts.out.unwrap_or_else(|| root.join(DEFAULT_BASELINE));
    // The artifact is committed with a trailing newline.
    if let Err(e) = std::fs::write(&out, format!("{json}\n")) {
        eprintln!("render3-sync baseline: writing {}: {e}", out.display());
        return ExitCode::FAILURE;
    }
    eprintln!(
        "render3-sync: recorded baseline ({} files, ref `{angular_ref}`) -> {}",
        baseline.files.len(),
        out.display()
    );
    ExitCode::SUCCESS
}

// ------------------------------------------------------------------------------------------------
// `drift`
// ------------------------------------------------------------------------------------------------

fn cmd_drift(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("render3-sync drift: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(root) = resolve_repo_root(opts.repo_root.clone()) else {
        eprintln!("render3-sync drift: could not locate repo root; pass --repo-root");
        return ExitCode::FAILURE;
    };

    let baseline_path = opts.baseline.unwrap_or_else(|| root.join(DEFAULT_BASELINE));
    let baseline: Baseline = match std::fs::read_to_string(&baseline_path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("render3-sync drift: parsing {}: {e}", baseline_path.display());
                return ExitCode::FAILURE;
            }
        },
        Err(e) => {
            eprintln!(
                "render3-sync drift: cannot read baseline {} ({e}); run `baseline` first",
                baseline_path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    let reader = FsReader::new(&root);
    let current_ref = opts.angular_ref.as_deref().unwrap_or("current");
    let report = match sync::diff_baseline(&reader, &baseline, current_ref) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("render3-sync drift: reading references failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("render3-sync drift: serialize failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{json}");
    if let Some(out) = opts.out
        && let Err(e) = std::fs::write(&out, &json)
    {
        eprintln!("render3-sync drift: writing {}: {e}", out.display());
        return ExitCode::FAILURE;
    }

    if report.is_clean() {
        eprintln!("render3-sync: CLEAN — render3 references still 1:1 with the baseline");
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "render3-sync: DRIFT — {} file(s), {} port task(s)",
            report.files.len(),
            report.tasks.len()
        );
        ExitCode::FAILURE
    }
}

// ------------------------------------------------------------------------------------------------
// `codegen-verify`
// ------------------------------------------------------------------------------------------------

fn cmd_codegen_verify(args: &[String]) -> ExitCode {
    let opts = match parse_opts(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("render3-sync codegen-verify: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(root) = resolve_repo_root(opts.repo_root.clone()) else {
        eprintln!("render3-sync codegen-verify: could not locate repo root; pass --repo-root");
        return ExitCode::FAILURE;
    };

    let reader = FsReader::new(&root);
    let report = match sync::verify_codegen(&reader) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("render3-sync codegen-verify: reading sources failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    let json = match serde_json::to_string_pretty(&report) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("render3-sync codegen-verify: serialize failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    println!("{json}");
    if let Some(out) = opts.out
        && let Err(e) = std::fs::write(&out, &json)
    {
        eprintln!("render3-sync codegen-verify: writing {}: {e}", out.display());
        return ExitCode::FAILURE;
    }

    let out_of_sync = report.out_of_sync();
    if out_of_sync.is_empty() {
        eprintln!(
            "render3-sync: IN SYNC — {} mechanical table(s) match the committed Rust",
            report.entries.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("render3-sync: {} mechanical row(s) OUT OF SYNC:", out_of_sync.len());
        for e in out_of_sync {
            eprintln!("  {} -> {}", e.ts_file, e.rust_file);
        }
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_opts_reads_all_flags() {
        let args: Vec<String> = ["--repo-root", "/r", "--baseline", "b.json", "--out", "o.json", "--ref", "v22"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let o = parse_opts(&args).unwrap();
        assert_eq!(o.repo_root, Some(PathBuf::from("/r")));
        assert_eq!(o.baseline, Some(PathBuf::from("b.json")));
        assert_eq!(o.out, Some(PathBuf::from("o.json")));
        assert_eq!(o.angular_ref.as_deref(), Some("v22"));
    }

    #[test]
    fn parse_opts_empty_is_default() {
        assert_eq!(parse_opts(&[]).unwrap(), Opts::default());
    }

    #[test]
    fn parse_opts_rejects_unknown_flag() {
        let args = vec!["--nope".to_string()];
        let err = parse_opts(&args).unwrap_err();
        assert!(err.contains("unknown flag --nope"), "{err}");
    }

    #[test]
    fn parse_opts_rejects_missing_value() {
        let args = vec!["--repo-root".to_string()];
        let err = parse_opts(&args).unwrap_err();
        assert!(err.contains("requires a value"), "{err}");
    }

    #[test]
    fn explicit_repo_root_is_used_verbatim() {
        let r = resolve_repo_root(Some(PathBuf::from("/explicit/root")));
        assert_eq!(r, Some(PathBuf::from("/explicit/root")));
    }
}
