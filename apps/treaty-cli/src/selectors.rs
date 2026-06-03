//! Project-wide SELECTOR REGISTRY scan for cross-module selector resolution.
//!
//! Treaty's Ivy compiler resolves a parent component's template tags to its child component
//! dependencies. For a CHILD declared in the SAME file it reads the sibling's real
//! `@Component`/`@Directive` `selector` directly; for a CHILD IMPORTED from ANOTHER module it
//! historically fell back to a class-name↔tag FOLDING convention (`<stat-card>` ↔ `class StatCard`).
//! That fold silently fails for the conventional Angular-CLI shape (`class StatCard` with
//! `selector: "app-stat-card"`, used as `<app-stat-card>`): the tag folds to `AppStatCard`, no
//! import is named that, and the child renders as an empty host.
//!
//! File I/O and module-path resolution are HOST/bundler concerns — NOT the compiler's. So the native
//! build (which already crawls the whole module graph) scans every first-party `.ts` ONCE to record
//! each `@Component`/`@Directive` class's real `selector`, then per file resolves that file's imports
//! to `{ localImportName -> selector }` and hands the compiler the resulting
//! [`treaty_ivy::source_compile::SelectorRegistry`]. The compiler stays "dumb": it never reads
//! another file; it only consumes the already-resolved name→selector mapping. The mapping is purely
//! ADDITIVE — absent it, the compiler's existing fold convention is used unchanged.

use std::collections::HashMap;
use std::path::Path;

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Class, Decorator, Expression, ImportDeclarationSpecifier, ImportOrExportKind, ObjectExpression,
    ObjectPropertyKind, PropertyKey, Statement,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

use treaty_ivy::source_compile::SelectorRegistry;

/// The project-wide map from a `@Component`/`@Directive` CLASS NAME to its declared `selector`
/// string, collected once by scanning every first-party `.ts` source. Keyed by class name because
/// that is exactly what a consumer's `import { ClassName } from '...'` binds.
pub type ProjectSelectors = HashMap<String, String>;

/// Whether a path is a `.ts` source the scanner should read for component/directive selectors.
/// `.tsx`/`.tjsx`/`.treaty` carry selectors too, but the registry's job is to fix the base-Angular
/// `.ts` cross-module case; the other front-ends already key on the filename convention.
pub fn is_scannable_ts(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("ts"))
}

/// Scan a single `.ts` SOURCE for every top-level `@Component`/`@Directive` class carrying a
/// non-empty string `selector`, recording `className -> selector` into `out`.
///
/// Parse failures are ignored (a file we cannot parse contributes nothing — the same tolerant stance
/// the rest of the build takes); a class with no selector, or a non-string selector expression, is
/// skipped (it cannot participate in CSS-selector matching).
pub fn scan_source_into(source: &str, out: &mut ProjectSelectors) {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, source, source_type).parse();
    if !ret.errors.is_empty() {
        return;
    }
    for stmt in &ret.program.body {
        let Some(class) = statement_class(stmt) else {
            continue;
        };
        let Some(id) = &class.id else { continue };
        let class_name = id.name.to_string();
        if let Some(selector) = component_or_directive_selector(class) {
            if !selector.trim().is_empty() {
                out.insert(class_name, selector);
            }
        }
    }
}

/// Build the per-file [`SelectorRegistry`] (`{ localImportName -> selector }`) for `source`, using the
/// already-scanned project-wide `className -> selector` map.
///
/// For each VALUE import specifier in `source` (`import { StatCard } from './card'`,
/// `import { StatCard as Card } from './card'`, default / namespace imports are not class-name
/// bindings and are skipped), the IMPORTED (module-exported) name is looked up in `project`; on a
/// hit, the LOCAL binding name is mapped to that selector. Type-only imports (`import type {…}` or an
/// inline `import { type Foo }`) are excluded — they are erased at emit and can never be a runtime
/// directive. Returns `None` when no import resolved to a known selector, so the caller passes `None`
/// (the compiler then uses its fold convention, byte-unchanged).
pub fn registry_for_source(source: &str, project: &ProjectSelectors) -> Option<SelectorRegistry> {
    if project.is_empty() {
        return None;
    }
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, source, source_type).parse();
    if !ret.errors.is_empty() {
        return None;
    }

    let mut registry = SelectorRegistry::new();
    for stmt in &ret.program.body {
        let Statement::ImportDeclaration(import) = stmt else {
            continue;
        };
        // `import type { … } from …` is wholly type-space.
        if import.import_kind == ImportOrExportKind::Type {
            continue;
        }
        let Some(specifiers) = &import.specifiers else {
            continue;
        };
        for spec in specifiers {
            let ImportDeclarationSpecifier::ImportSpecifier(s) = spec else {
                // Default / namespace imports do not bind a `@Component` class by its exported name.
                continue;
            };
            // Inline `import { type Bar }`: type-only specifier.
            if s.import_kind == ImportOrExportKind::Type {
                continue;
            }
            // The MODULE-EXPORTED name (what the project map is keyed on) vs the LOCAL binding name
            // (what the parent's template / `dependencies` references). `import { A as B }` →
            // imported `A`, local `B`.
            let imported_name = s.imported.name();
            let local_name = s.local.name.as_str();
            if let Some(selector) = project.get(imported_name.as_str()) {
                registry.insert(local_name.to_string(), selector.clone());
            }
        }
    }

    if registry.is_empty() {
        None
    } else {
        Some(registry)
    }
}

/// Pull the class out of a top-level statement (plain / exported / default-exported declaration).
fn statement_class<'a>(stmt: &'a Statement<'a>) -> Option<&'a Class<'a>> {
    match stmt {
        Statement::ClassDeclaration(c) => Some(c.as_ref()),
        Statement::ExportNamedDeclaration(export) => match &export.declaration {
            Some(oxc_ast::ast::Declaration::ClassDeclaration(c)) => Some(c.as_ref()),
            _ => None,
        },
        Statement::ExportDefaultDeclaration(export) => {
            if let oxc_ast::ast::ExportDefaultDeclarationKind::ClassDeclaration(c) =
                &export.declaration
            {
                Some(c.as_ref())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The `selector` string of a class's `@Component`/`@Directive` decorator, if it carries one with a
/// string-literal value. Returns `None` for a class with no such decorator, no `selector`, or a
/// non-string `selector` expression.
fn component_or_directive_selector(class: &Class) -> Option<String> {
    for dec in &class.decorators {
        let name = decorator_name(dec)?;
        if name != "Component" && name != "Directive" {
            continue;
        }
        let obj = decorator_object(dec)?;
        if let Some(selector) = string_prop(obj, "selector") {
            return Some(selector);
        }
    }
    None
}

/// The callee identifier of a decorator, whether `@Foo` (identifier) or `@Foo({...})` (call).
fn decorator_name<'a>(dec: &'a Decorator<'a>) -> Option<&'a str> {
    match &dec.expression {
        Expression::CallExpression(call) => match &call.callee {
            Expression::Identifier(id) => Some(id.name.as_str()),
            _ => None,
        },
        Expression::Identifier(id) => Some(id.name.as_str()),
        _ => None,
    }
}

/// The first `@Foo({...})` decorator argument object, if the decorator is a call with an object
/// literal first argument.
fn decorator_object<'a>(dec: &'a Decorator<'a>) -> Option<&'a ObjectExpression<'a>> {
    let Expression::CallExpression(call) = &dec.expression else {
        return None;
    };
    let first = call.arguments.first()?;
    match first.as_expression()? {
        Expression::ObjectExpression(obj) => Some(obj.as_ref()),
        _ => None,
    }
}

/// The string-literal value of property `key` on an object literal, if present and a string.
fn string_prop(obj: &ObjectExpression, key: &str) -> Option<String> {
    for prop in &obj.properties {
        let ObjectPropertyKind::ObjectProperty(p) = prop else {
            continue;
        };
        let name = match &p.key {
            PropertyKey::StaticIdentifier(id) => id.name.as_str(),
            PropertyKey::StringLiteral(s) => s.value.as_str(),
            _ => continue,
        };
        if name != key {
            continue;
        }
        if let Expression::StringLiteral(s) = &p.value {
            return Some(s.value.to_string());
        }
    }
    None
}

/// Scan a whole set of already-read sources into one project-wide selector map. Convenience for the
/// native build, which has the graph's sources in hand.
pub fn scan_sources<'a>(sources: impl IntoIterator<Item = &'a str>) -> ProjectSelectors {
    let mut out = ProjectSelectors::new();
    for source in sources {
        scan_source_into(source, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT_CARD: &str = r#"
        import { Component, input } from '@angular/core';
        @Component({ selector: 'app-stat-card', template: '<p>{{label()}}</p>' })
        export class StatCard { readonly label = input(''); }
    "#;

    const DASHBOARD: &str = r#"
        import { Component } from '@angular/core';
        import { StatCard } from '../shared/stat-card';
        @Component({
            selector: 'app-dashboard',
            imports: [StatCard],
            template: '<app-stat-card label="x"></app-stat-card>',
        })
        export class Dashboard {}
    "#;

    #[test]
    fn scans_component_selector_by_class_name() {
        let project = scan_sources([STAT_CARD]);
        assert_eq!(project.get("StatCard").map(String::as_str), Some("app-stat-card"));
    }

    #[test]
    fn scans_directive_selector() {
        let src = r#"
            import { Directive } from '@angular/core';
            @Directive({ selector: '[themeToggle]' })
            export class ThemeToggle {}
        "#;
        let project = scan_sources([src]);
        assert_eq!(project.get("ThemeToggle").map(String::as_str), Some("[themeToggle]"));
    }

    #[test]
    fn builds_per_file_registry_from_imports() {
        let project = scan_sources([STAT_CARD]);
        let registry = registry_for_source(DASHBOARD, &project).expect("registry");
        assert_eq!(registry.get("StatCard").map(String::as_str), Some("app-stat-card"));
    }

    #[test]
    fn aliased_import_keys_on_local_name() {
        let project = scan_sources([STAT_CARD]);
        let dashboard = r#"
            import { Component } from '@angular/core';
            import { StatCard as Card } from '../shared/stat-card';
            @Component({ selector: 'app-dashboard', imports: [Card], template: '<app-stat-card/>' })
            export class Dashboard {}
        "#;
        let registry = registry_for_source(dashboard, &project).expect("registry");
        // Keyed by the LOCAL binding name `Card`, valued by StatCard's real selector.
        assert_eq!(registry.get("Card").map(String::as_str), Some("app-stat-card"));
        assert!(registry.get("StatCard").is_none(), "must key on local name, not exported name");
    }

    #[test]
    fn type_only_import_is_excluded() {
        let project = scan_sources([STAT_CARD]);
        let src = r#"
            import type { StatCard } from '../shared/stat-card';
            export const x = 1;
        "#;
        assert!(registry_for_source(src, &project).is_none(), "type-only import must not register");
    }

    #[test]
    fn no_matching_import_yields_none() {
        let project = scan_sources([STAT_CARD]);
        let src = "import { Component } from '@angular/core'; export const x = 1;";
        assert!(registry_for_source(src, &project).is_none());
    }
}

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
