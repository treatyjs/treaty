use swc_core::ecma::{
    ast::Program,
    transforms::testing::test_inline,
    visit::{as_folder, FoldWith, VisitMut},
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
                if let ExprOrSuper::Expr(expr) = &call_expr.callee {
                    if let Expr::Ident(ident) = &**expr {
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
                            // Serialize the call expression arguments to JSON
                            let args_json = serde_json::to_string(&call_expr.args).unwrap_or_default();

                            match handler.parse(&args_json) {
                                Ok(parsed_meta) => {
                                    // Decide between JIT and AOT based on options
                                    match self.options.compilation_mode {
                                        CompilationMode::JIT => {
                                            handler.transform_to_JIT(&parsed_meta);
                                        },
                                        CompilationMode::AOT => {
                                            handler.transform_to_ivy(&parsed_meta);
                                        }
                                    }
                                },
                                Err(e) => eprintln!("Error parsing Angular metadata: {}", e),
                            }
                            to_remove.push(i);
                        }
                    }
                }
            }
        }

        for index in to_remove.iter().rev() {
            n.decorators.remove(*index);
        }

        n.visit_mut_children_with(self);
    }
}

#[plugin_transform]
pub fn process_transform(program: Program, metadata: TransformPluginProgramMetadata) -> Program {
    let options: PluginOptions = serde_json::from_str(&metadata.plugin_config)
        .unwrap_or_else(|_| PluginOptions { compilation_mode: CompilationMode::JIT }); 

    program.fold_with(&mut as_folder(TransformVisitor::new(options)))
}
