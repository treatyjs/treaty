use std::{env, path::Path};
mod parser;
use oxc_allocator::Allocator;
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::parser::parse_mixed_content;

// Instruction:
// create a `test.js`,
// run `cargo run -p oxc_parser --example parser`
// or `cargo watch -x "run -p oxc_parser --example parser"`

fn main() -> Result<(), String> {
    let name = env::args().nth(1).unwrap_or_else(|| "test.js".to_string());
    let path = Path::new(&name);
    let source_text = std::fs::read_to_string(path).map_err(|_| format!("Missing '{name}'"))?;
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap();

    match parse_mixed_content(&source_text) {
        Ok(extracted) => {
            let javascript = extracted.javascript.join("\n");
            println!("HTML Content:\n{}", extracted.html.join("\n"));
            println!("CSS Content:\n{}", extracted.css.join("\n"));
            println!("JavaScript Content:\n{}", javascript);

            let ret = Parser::new(&allocator, &javascript, source_type).parse();

            println!("AST:");
            println!("{}", serde_json::to_string_pretty(&ret.program).unwrap());

            if ret.errors.is_empty() {
                println!("Parsed Successfully.");
            } else {
                for error in ret.errors {
                    let error = error.with_source_code(source_text.clone());
                    println!("{error:?}");
                    println!("Parsed with Errors.");
                }
            }
        }
        Err(e) => println!("Error parsing content: {}", e),
    }

    Ok(())
}
