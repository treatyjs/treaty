//! Angular template binding-expression pipeline: `{{ }}`, `[x]`, `(y)`, microsyntax.
//! lexer → ast → parser. Ported from `tools/angular-ref/packages/compiler/src/expression_parser/`.

pub mod ast;
pub mod lexer;
pub mod parser;
