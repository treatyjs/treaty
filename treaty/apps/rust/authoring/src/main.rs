use std::{
    env,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};
mod angular;
mod html;
mod parser;
mod treaty;

use self::angular::Angular;
use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

use crate::parser::parse_mixed_content;

fn main() {
    let name = env::args().nth(1).unwrap_or_else(|| "test.ts".to_string());
    let path: &Path = Path::new(&name);
    let source_text = std::fs::read_to_string(path).expect("{name} not found");

    let parse_content = parse_mixed_content(&source_text);

    println!("Parsed HTML Content:");
    parse_content.html.into_iter().for_each(|node| {
        println!("{}", node);
    });
    println!("CSS Content:\n{}", parse_content.css.join("\n"));

    let javascript = parse_content.javascript.join("\n");

    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap();

    let ret = Parser::new(&allocator, &javascript, source_type).parse();

    if !ret.errors.is_empty() {
        for error in ret.errors {
            let error = error.with_source_code(javascript.clone());
            println!("{error:?}");
        }
        return;
    }

    println!("Original:\n");
    println!("{javascript}\n");

     let semantic = SemanticBuilder::new(&javascript, source_type)
        .with_trivias(ret.trivias)
        .build_module_record(PathBuf::new(), &ret.program)
        .build(&ret.program)
        .semantic;

    let program = allocator.alloc(ret.program);
    Angular::new(&allocator, source_type, semantic)
        .build(program)
        .unwrap();

    let printed = Codegen::<false>::new(&javascript, CodegenOptions::default())
        .build(program)
        .source_text;
    println!("Transformed:\n");
    println!("{printed}");
    let gen_path = path.with_file_name(format!(
        "{}_gen{}",
        path.file_stem().unwrap_or_default().to_string_lossy(),
        path.extension()
            .map_or("".into(), |ext| format!(".{}", ext.to_string_lossy()))
    ));
    let mut file = File::create(gen_path).expect("Failed to create file");
    file.write_all(printed.as_bytes()).unwrap();
}

