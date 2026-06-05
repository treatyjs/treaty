//! Project-wide SELECTOR REGISTRY scan for cross-module selector resolution.
//!
//! The scanner now lives in [`rust_authoring::selectors`] so it is SHARED between the native
//! `treaty build` graph crawl (this crate) and the bundler-plugin NAPI addon
//! (`buildSelectorRegistry` / `buildImportedSelectors`) — both drive the exact same scan, so a
//! `@treaty/vite` build resolves cross-module selectors identically to a native build. This module is
//! a thin re-export so every existing `crate::selectors::…` reference (and the native build's two-pass
//! crawl in [`crate::native_build`]) keeps compiling unchanged.
//!
//! See [`rust_authoring::selectors`] for the WHY: the conventional Angular-CLI shape
//! (`class StatCard` with `selector: "app-stat-card"`, used as `<app-stat-card>`) does not fold, so it
//! resolves only through this registry; absent the registry the compiler's fold convention is used
//! unchanged (the mapping is purely ADDITIVE).

pub use rust_authoring::selectors::{
    is_scannable_ts, registry_for_source, scan_dir, scan_source_into, scan_sources, ProjectSelectors,
};

// ---------------------------------------------------------------------------
// CONVENTIONAL-SELECTOR REGRESSION PROOF (the real ng-bench-app).
//
// ng-bench-app's `dashboard.ts` imports `StatCard` (class name) and uses it by the CONVENTIONAL
// Angular-CLI element selector `<app-stat-card>` (`stat-card.ts` declares `selector:
// "app-stat-card"`). The class name `StatCard` does NOT fold to the tag `app-stat-card`, so this
// resolves ONLY through the cross-module selector registry — the exact case real selector resolution
// exists for. This test reads the SHIPPED example files (not an inline copy) so a future edit that
// breaks cross-module resolution — or reverts the example to a fold-aligned selector — fails here.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod ngbench_conventional_selector_regression {
    use super::*;
    use std::path::Path;

    fn ngbench_app_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/ng-bench-app/src/app")
    }

    #[test]
    fn dashboard_resolves_imported_app_stat_card_via_registry() {
        let root = ngbench_app_dir();
        let stat = std::fs::read_to_string(root.join("shared/stat-card.ts")).unwrap();
        let dash = std::fs::read_to_string(root.join("features/dashboard/dashboard.ts")).unwrap();
        let pipe = std::fs::read_to_string(root.join("shared/currency-format.pipe.ts")).unwrap();

        // The example child is in the conventional Angular-CLI form (NON-folding selector).
        assert!(
            stat.contains("selector: 'app-stat-card'"),
            "regression: stat-card.ts must keep the conventional `app-stat-card` selector"
        );
        assert!(
            dash.contains("<app-stat-card"),
            "regression: dashboard.ts must use the conventional `<app-stat-card>` tag"
        );

        // Project scan picks up StatCard's real selector; the per-file registry maps the import.
        let project = scan_sources([stat.as_str(), dash.as_str(), pipe.as_str()]);
        assert_eq!(project.get("StatCard").map(String::as_str), Some("app-stat-card"));
        let reg = registry_for_source(&dash, &project).expect("dashboard registry");
        assert_eq!(reg.get("StatCard").map(String::as_str), Some("app-stat-card"));

        // WITHOUT the registry: the fold convention cannot match `<app-stat-card>` to `StatCard`, so
        // the dependency is NOT discovered (the documented bug).
        let without = rust_authoring::angular_source::compile_angular_source_with_registry(
            &dash,
            "dashboard.ts",
            None,
        );
        assert!(
            !without.code.contains("dependencies: [StatCard"),
            "fold-only path must NOT resolve <app-stat-card> to StatCard; got: {}",
            without.code
        );

        // WITH the registry: StatCard lands in `dependencies`, and the 3 `<app-stat-card>` tags are
        // emitted as element instructions bound to that dependency — the parent now renders 3 stat
        // cards instead of 3 empty hosts.
        let with = rust_authoring::angular_source::compile_angular_source_with_registry(
            &dash,
            "dashboard.ts",
            Some(&reg),
        );
        assert!(with.errors.is_empty(), "errors: {:?}", with.errors);
        assert!(
            with.code.contains("dependencies: [StatCard"),
            "registry must resolve StatCard into dependencies; got: {}",
            with.code
        );
        let stat_card_tags = with.code.matches("\"app-stat-card\"").count();
        assert_eq!(stat_card_tags, 3, "expected 3 app-stat-card element instructions (statCards=3)");
    }
}
