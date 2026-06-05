//! Thin, deterministic oxc wrapper: parse an Angular TypeScript source and extract the names of
//! its top-level **exported** symbols (classes, functions, enums, const/let/var, interfaces, type
//! aliases). This is the primitive the drift differ and the mechanical codegen build on — given
//! two refs of a file, the set difference of these names is the export-level drift.
//!
//! It deliberately does NO transpilation; it only reads structure. Anything it cannot classify is
//! simply not reported as an export (determinism over guessing).

use oxc_allocator::Allocator;
use oxc_ast::ast::{BindingPattern, Declaration, Statement};
use oxc_parser::Parser;
use oxc_span::SourceType;

/// The kind of an exported top-level symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportKind {
    Class,
    Function,
    Enum,
    Variable,
    Interface,
    TypeAlias,
}

/// One exported top-level symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedSymbol {
    pub name: String,
    pub kind: ExportKind,
}

/// Parse `source` as TypeScript and return its top-level exported symbols, sorted by name. The
/// parse is best-effort: a TS source with recoverable errors still yields whatever exports parsed.
pub fn exported_symbols(source: &str) -> Vec<ExportedSymbol> {
    let allocator = Allocator::default();
    let source_type = SourceType::default().with_typescript(true);
    let ret = Parser::new(&allocator, source, source_type).parse();

    let mut out = Vec::new();
    for stmt in &ret.program.body {
        let Statement::ExportNamedDeclaration(export) = stmt else {
            continue;
        };
        let Some(decl) = &export.declaration else {
            // `export { Foo, Bar };` re-export / specifier form.
            for spec in &export.specifiers {
                out.push(ExportedSymbol {
                    name: spec.exported.name().to_string(),
                    kind: ExportKind::Variable,
                });
            }
            continue;
        };
        collect_declaration(decl, &mut out);
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup();
    out
}

fn collect_declaration(decl: &Declaration, out: &mut Vec<ExportedSymbol>) {
    match decl {
        Declaration::ClassDeclaration(c) => {
            if let Some(id) = &c.id {
                out.push(ExportedSymbol {
                    name: id.name.to_string(),
                    kind: ExportKind::Class,
                });
            }
        }
        Declaration::FunctionDeclaration(f) => {
            if let Some(id) = &f.id {
                out.push(ExportedSymbol {
                    name: id.name.to_string(),
                    kind: ExportKind::Function,
                });
            }
        }
        Declaration::TSEnumDeclaration(e) => {
            out.push(ExportedSymbol {
                name: e.id.name.to_string(),
                kind: ExportKind::Enum,
            });
        }
        Declaration::TSInterfaceDeclaration(i) => {
            out.push(ExportedSymbol {
                name: i.id.name.to_string(),
                kind: ExportKind::Interface,
            });
        }
        Declaration::TSTypeAliasDeclaration(t) => {
            out.push(ExportedSymbol {
                name: t.id.name.to_string(),
                kind: ExportKind::TypeAlias,
            });
        }
        Declaration::VariableDeclaration(v) => {
            for d in &v.declarations {
                if let BindingPattern::BindingIdentifier(id) = &d.id {
                    out.push(ExportedSymbol {
                        name: id.name.to_string(),
                        kind: ExportKind::Variable,
                    });
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_each_export_kind() {
        let src = r#"
            export class Identifiers {}
            export function compileComponentFromMetadata() {}
            export enum AttributeMarker { NamespaceURI = 0, Classes = 1 }
            export const FOO = 1;
            export interface Target {}
            export type Node = Element | Template;
            class NotExported {}
        "#;
        let syms = exported_symbols(src);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Identifiers"));
        assert!(names.contains(&"compileComponentFromMetadata"));
        assert!(names.contains(&"AttributeMarker"));
        assert!(names.contains(&"FOO"));
        assert!(names.contains(&"Target"));
        assert!(names.contains(&"Node"));
        assert!(!names.contains(&"NotExported"));
    }

    #[test]
    fn handles_export_specifiers() {
        let src = "const a = 1; const b = 2; export { a, b };";
        let names: Vec<String> = exported_symbols(src).into_iter().map(|s| s.name).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
    }
}
