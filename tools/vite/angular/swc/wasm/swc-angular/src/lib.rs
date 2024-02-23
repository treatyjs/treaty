mod angular;
mod plugin;
mod classes;


use crate::{
    angular::{NgTraitHandlerFactory, NgTrait},
    plugin::{PluginOptions, CompilationMode}
};
use swc_core::ecma::{
    ast::{Program, Expr, Class, MemberProp},
    visit::{as_folder, FoldWith, VisitMut, VisitMutWith},
};
use swc_core::plugin::{plugin_transform, proxies::TransformPluginProgramMetadata};
use serde_json;



pub struct TransformVisitor {
    options: PluginOptions, 
}

impl TransformVisitor {
    pub fn new(options: PluginOptions) -> Self {
        Self { options }
    }
}


impl VisitMut for TransformVisitor {
    fn visit_mut_class(&mut self, n: &mut Class) {
        let mut to_remove = vec![];
        for (i, decorator) in n.decorators.iter_mut().enumerate() {
            if let Expr::Call(call_expr) = &mut *decorator.expr {
                // Directly pattern match on the callee without dereferencing
                match &call_expr.callee {
                    swc_ecma_ast::Callee::Expr(expr) => {
                        // Now match on the expression pointed to by the callee
                        if let Expr::Member(member_expr) = &**expr {
                            if let MemberProp::Ident(ident) = &member_expr.prop {
                                let trait_type = match ident.sym.as_ref() {
                                    "Component" => Some(NgTrait::Component),
                                    "Directive" => Some(NgTrait::Directive),
                                    "Injectable" => Some(NgTrait::Injectable),
                                    "NgModule" => Some(NgTrait::NgModule),
                                    "Pipe" => Some(NgTrait::Pipe),
                                    _ => None,
                                };

                                if let Some(trait_type) = trait_type {
                                    let handler = NgTraitHandlerFactory::get_handler(&trait_type);
                                    
                                    // Assume parse method is adapted to handle new AST structure
                                    match handler.parse(call_expr) {
                                        Ok(parsed_meta) => {
                                            let transform_result = match self.options.compilation_mode {
                                                CompilationMode::JIT => handler.transform_to_jit(&parsed_meta, n),
                                                CompilationMode::AOT => handler.transform_to_ivy(&parsed_meta, n),
                                            };
                                            if let Err(e) = transform_result {
                                                eprintln!("Error transforming Angular metadata: {}", e);
                                            }
                                        },
                                        Err(e) => eprintln!("Error parsing Angular metadata: {}", e),
                                    }
                                    to_remove.push(i);
                                }
                            }
                        }
                    },
                    _ => {} // Handle other callee types as necessary
                }
            }
        }

        // Remove decorators that have been processed
        for index in to_remove.iter().rev() {
            n.decorators.remove(*index);
        }

        // Continue traversing the AST
        n.visit_mut_children_with(self);
    }
}


#[plugin_transform]
pub fn process_transform(program: Program, metadata: TransformPluginProgramMetadata) -> Program {
    let options: PluginOptions = serde_json::from_str(&metadata.plugin_config)
        .unwrap_or_else(|_| PluginOptions { compilation_mode: CompilationMode::JIT }); 

    program.fold_with(&mut as_folder(TransformVisitor::new(options)))
}
