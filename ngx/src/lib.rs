#![allow(clippy::not_unsafe_ptr_arg_deref)]
use swc_common::{SourceMapper, Spanned};
use swc_core::{
    common::FileName,
    ecma::ast::Program,
    plugin::{
        metadata::TransformPluginMetadataContextKind,
        plugin_transform,
        proxies::{PluginCommentsProxy, TransformPluginProgramMetadata},
    },
};

#[plugin_transform]
fn treaty_authoring(program: Program, data: TransformPluginProgramMetadata) -> Program {


    let file_name = match data.get_context(&TransformPluginMetadataContextKind::Filename) {
        Some(s) => FileName::Real(s.into()),
        None => FileName::Anon,
    };

    let pos = data.source_map.lookup_char_pos(program.span().lo);
    let hash = pos.file.src_hash;

    let mut pass =
        treaty_authoring::treaty_authoring(file_name, hash, PluginCommentsProxy);

    // program.visit_mut_with(&mut pass);

    program
}