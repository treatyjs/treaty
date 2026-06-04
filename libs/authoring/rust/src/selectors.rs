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
//!
//! This module is the SHARED scanner: the Rust `treaty build` graph crawl (via `treaty_cli`, which
//! re-exports this module) and the bundler-plugin NAPI addon (`buildSelectorRegistry` /
//! `buildImportedSelectors`) both drive the exact same scan, so a `@treaty/vite` build resolves
//! cross-module selectors identically to a native build.

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

/// Recursively scan every first-party `.ts` file under `root_dir` into one project-wide selector
/// map. The bundler-plugin NAPI shim drives the cold-build prewarm through this entry: it walks the
/// directory tree itself (host file I/O is a build concern, not the compiler's), reads each
/// `.ts` source, and records its `@Component`/`@Directive` selectors. `node_modules` and dot-dirs are
/// skipped so only first-party sources contribute — published partials never own a conventional
/// authoring selector here. A `root_dir` that does not exist yields an empty map (no error), matching
/// the tolerant stance the rest of the scan takes.
pub fn scan_dir(root_dir: &Path) -> ProjectSelectors {
    let mut out = ProjectSelectors::new();
    scan_dir_into(root_dir, &mut out);
    out
}

/// Walk `dir` recursively, scanning every first-party `.ts` source into `out`. Skips `node_modules`
/// and any dot-directory (`.git`, `.angular`, …). Read/parse failures on individual files are
/// tolerated (they simply contribute nothing).
fn scan_dir_into(dir: &Path, out: &mut ProjectSelectors) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Never descend into dependency or tool-cache trees: only first-party authoring sources
            // own a conventional selector, and the partials in `node_modules` would only add noise.
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            scan_dir_into(&path, out);
        } else if file_type.is_file() && is_scannable_ts(&path) {
            if let Ok(source) = std::fs::read_to_string(&path) {
                scan_source_into(&source, out);
            }
        }
    }
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

    #[test]
    fn scan_dir_walks_first_party_ts_and_skips_node_modules() {
        let tmp = std::env::temp_dir().join(format!("treaty-selscan-{}", std::process::id()));
        let app = tmp.join("src/app/shared");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("stat-card.ts"), STAT_CARD).unwrap();
        // A node_modules partial must NOT contribute (it would shadow the first-party selector).
        let nm = tmp.join("node_modules/@angular/material");
        std::fs::create_dir_all(&nm).unwrap();
        std::fs::write(
            nm.join("decoy.ts"),
            "import { Component } from '@angular/core'; @Component({ selector: 'mat-decoy', template: '' }) export class Decoy {}",
        )
        .unwrap();

        let project = scan_dir(&tmp);
        assert_eq!(project.get("StatCard").map(String::as_str), Some("app-stat-card"));
        assert!(project.get("Decoy").is_none(), "node_modules sources must be skipped");

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn scan_dir_missing_root_yields_empty() {
        let project = scan_dir(Path::new("definitely/not/a/real/dir/anywhere"));
        assert!(project.is_empty());
    }
}
