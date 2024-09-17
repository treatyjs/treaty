use std::env;
use std::path::PathBuf;

mod treaty;
use treaty::lexer::Lexer;
use treaty::parser::Parser;
use treaty::token::TokenKind;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let default_path = "apps/rust/authoring/src/test.treaty";
    let file_path = env::args().nth(1).unwrap_or_else(|| default_path.to_string());

    let path = PathBuf::from(&file_path);
    println!("Reading file: {}", path.display());

    let source_text = std::fs::read_to_string(&path)
        .map_err(|e| format!("Error reading {}: {}", path.display(), e))?;

    println!("Lexing source text...");
    let mut lexer = Lexer::new(&source_text);
    let mut tokens = Vec::new();

    while let Some(token) = lexer.next_token() {
        tokens.push(token);
    }

    let mut javascript_chunks = Vec::new();
    let mut html_chunks = Vec::new();
    let mut css_chunks = Vec::new();

    for token in &tokens {
        match &token.kind {
            TokenKind::JavaScript(code) => javascript_chunks.push(code.clone()),
            TokenKind::HTML(content) => html_chunks.push(content.clone()),
            TokenKind::Style(style) => css_chunks.push(style.clone()),
            _ => {}
        }
    }

    println!("JavaScript Chunks:");
    for chunk in &javascript_chunks {
        println!("{}", chunk);
    }

    println!("\nHTML Chunks:");
    for chunk in &html_chunks {
        println!("{}", chunk);
    }

    println!("\nCSS Chunks:");
    for chunk in &css_chunks {
        println!("{}", chunk);
    }

    println!("\nParsing tokens...");
    let mut parser = Parser::new(tokens);
    let ast = parser.parse();

    println!("AST:");
    println!("{:#?}", ast);

    Ok(())
}