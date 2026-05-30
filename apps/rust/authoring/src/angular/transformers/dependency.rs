use crate::angular::context::AngularContext;
use crate::angular::TopLevelDecorator;
use oxc::syntax::operator::LogicalOperator;
use oxc_span::SPAN;
use std::rc::Rc;

use oxc_ast::{ast::*, AstBuilder, AstKind};

use crate::angular::{context::AngularCtx, InjectableOptions, ParamDecorator};

pub struct DependencyInjection<'a> {
    ast: Rc<AstBuilder<'a>>,
    nodes: oxc_allocator::Vec<'a, AstKind<'a>>,
    context: AngularCtx<'a>,
    constructor_params: Vec<ParamDecorator>,
}

impl<'a> DependencyInjection<'a> {
    pub fn new(ast: Rc<AstBuilder<'a>>, context: AngularCtx<'a>) -> Option<Self> {
        let nodes: oxc_allocator::Vec<'_, AstKind<'_>> = ast.vec();
        Some(Self {
            ast,
            nodes,
            context,
            constructor_params: Vec::new(),
        })
    }

    pub fn specifier_to_remove() -> Vec<&'a str> {
        vec!["inject"]
    }

    pub fn transform_class(&mut self, class: &mut Class<'a>, top_level_decorators: &Vec<(TopLevelDecorator, usize)>) {
        let has_decorator = !class.decorators.is_empty();
        let class_name = if has_decorator {
            class
                .id
                .clone()
                .map(|id| self.context.generate_uid(id.name.as_str()))
                .or_else(|| Some(self.context.generate_uid("class")))
        } else {
            None
        };
        println!("{:#?}", class_name);

        if let Some(constructor) = class.body.body.iter_mut().find_map(|element| {
            if let ClassElement::MethodDefinition(method_def) = element {
                if method_def.kind == MethodDefinitionKind::Constructor {
                    return Some(method_def);
                }
            }
            None
        }) {
            // Now constructor is a mutable reference to the MethodDefinition of the constructor.
            let constructor_has_params = !constructor.value.params.is_empty();

            if constructor_has_params {
                // Use the std::mem::replace strategy if direct mutation isn't possible due to the Vec's traits.
                let mut temp_params =
                    std::mem::replace(&mut constructor.value.params.items, self.ast.vec());

                for param in temp_params.iter_mut() {
                    let mut decorators_to_remove = Vec::new();
                    let mut found_param_decorator = false;
                    for (index, decorator) in param.decorators.iter().enumerate() {
                        let identifier_name =
                            if let Expression::CallExpression(boxed_expr) = &decorator.expression {
                                if let Expression::Identifier(identifier) = &boxed_expr.callee {
                                    Some(identifier.name.as_str())
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                        if let Some(name) = identifier_name {
                            if let Some(param_decorator) =
                                ParamDecorator::from_str(name, param, decorator)
                            {
                                match &param_decorator {
                                    ParamDecorator::Inject(_name)
                                    | ParamDecorator::ASelf(_name)
                                    | ParamDecorator::Optional(_name)
                                    | ParamDecorator::SkipSelf(_name) => {
                                        found_param_decorator = true;
                                        self.constructor_params.push(param_decorator);
                                        decorators_to_remove.push(index);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    for index in decorators_to_remove.into_iter().rev() {
                        param.decorators.remove(index);
                    }

                    if !found_param_decorator {
                        if let Some(type_name) = self.extract_type_name_from_type_annotation(
                            &param.type_annotation.as_ref(),
                        ) {
                            self.constructor_params
                                .push(ParamDecorator::Inject(type_name));
                        }
                    }
                }
                constructor.value.params.items = temp_params;
            }
        }

        let factory_name = format!("factory{}", class_name.unwrap_or_default());
        // Iterate over top_level_decorators and remove processed decorators
        for (decorator, _) in top_level_decorators.into_iter() {
            match decorator {
                TopLevelDecorator::Injectable { options } => {
                    println!("Processing Injectable decorator with options: {:?}", options);
                    let property_definition = self.ng_factory_builder(factory_name.clone(), Some(&options));
                    class.body.body.insert(0, property_definition);
                }
                _ => {
                    let property_definition = self.ng_factory_builder(factory_name.clone(), None);
                    class.body.body.insert(0, property_definition);
                }
            }
        }
    }

    fn extract_type_name_from_type_annotation(
        &self,
        type_annotation: &Option<&oxc_allocator::Box<oxc_ast::ast::TSTypeAnnotation>>,
    ) -> Option<String> {
        type_annotation.as_ref().and_then(|ta| {
            match &ta.type_annotation {
                TSType::TSTypeReference(tstype_ref) => {
                    // Directly handle the type reference to extract the type name
                    self.extract_type_name_from_tstype_name(&tstype_ref.type_name)
                }
                // Potentially handle other TSType variants if needed
                _ => None,
            }
        })
    }

    fn extract_type_name_from_tstype_name(&self, tstype_name: &TSTypeName) -> Option<String> {
        match tstype_name {
            TSTypeName::IdentifierReference(identifier_ref) => {
                // Direct extraction of the name from IdentifierReference
                Some(identifier_ref.name.to_string())
            }
            TSTypeName::QualifiedName(qualified_name) => {
                // For QualifiedName, handle the left and right parts
                let left_name = self.extract_type_name_from_tstype_name(&qualified_name.left)?;
                Some(format!("{}.{}", left_name, qualified_name.right.name))
            }
            // OXC 0.133 added TSTypeName::ThisExpression (and may add more); not a named type ref.
            _ => None,
        }
    }

    fn ng_factory_builder(
        &self,
        factory_name: String,
        injectable_options: Option<&InjectableOptions>,
    ) -> ClassElement<'a> {
        let identifier_span = SPAN;
        let identifier_key = self
            .ast
            .property_key_static_identifier(identifier_span, "ɵfac");

        let function_identifier_name = self.ast.ident(&factory_name);
        let function_identifier_ref = self
            .ast
            .binding_identifier(identifier_span, function_identifier_name);
        let function_identifier = self
            .ast
            .expression_identifier(identifier_span, self.ast.ident(&factory_name));

        let param_identifier_expression =
            self.ast.expression_identifier(identifier_span, "t");

        let param_binding_pattern = self
            .ast
            .binding_pattern_binding_identifier(identifier_span, "t");

        let formal_parameter = self.ast.formal_parameter(
            identifier_span,
            self.ast.vec(),
            param_binding_pattern,
            None::<oxc_allocator::Box<'_, TSTypeAnnotation<'_>>>,
            None::<oxc_allocator::Box<'_, Expression<'_>>>,
            false,
            None,
            false,
            false,
        );

        let params: FormalParameters<'_> = self.ast.formal_parameters(
            identifier_span,
            FormalParameterKind::FormalParameter,
            self.ast.vec1(formal_parameter),
            None::<oxc_allocator::Box<'_, FormalParameterRest<'_>>>,
        );

        // Collecting the injection tokens
        let inject_tokens: Vec<String> = self
            .constructor_params
            .iter()
            .filter_map(|param| {
                if let ParamDecorator::Inject(token) = param {
                    Some(token.clone())
                } else {
                    None
                }
            })
            .collect();

        // Prepare a vector to hold all call expressions as arguments for the new expression
        let mut new_expression_arguments = self
            .ast
            .vec_with_capacity::<Argument<'a>>(inject_tokens.len());

        for token in inject_tokens {
            let token_identifier = self
                .ast
                .expression_identifier(identifier_span, self.ast.ident(&token));

            let i0_identifier = self.ast.expression_identifier(identifier_span, "i0");

            // Create the call expression for each token
            let inject_call_expression = self.ast.expression_call(
                identifier_span,
                Expression::from(self.ast.member_expression_static(
                    identifier_span,
                    i0_identifier,
                    self.ast.identifier_name(identifier_span, "ɵɵinject"),
                    false,
                )),
                None::<oxc_allocator::Box<'_, TSTypeParameterInstantiation<'_>>>,
                self.ast.vec1(Argument::from(token_identifier)),
                false,
            );

            // Add the call expression as an argument to the new_expression_arguments
            new_expression_arguments.push(Argument::from(inject_call_expression));
        }

        // Creating the new expression with the aggregated arguments
        let new_expression = self.ast.expression_new(
            identifier_span,
            self.ast.expression_parenthesized(
                identifier_span,
                self.ast.expression_logical(
                    identifier_span,
                    param_identifier_expression,
                    LogicalOperator::Or,
                    function_identifier,
                ),
            ),
            None::<oxc_allocator::Box<'_, TSTypeParameterInstantiation<'_>>>,
            new_expression_arguments, // Use the aggregated call expressions here
        );

        let return_statement = self
            .ast
            .statement_return(identifier_span, Some(new_expression));

        let function_body = self.ast.function_body(
            identifier_span,
            self.ast.vec(),
            self.ast.vec1(return_statement),
        );

        let function_expression = self.ast.expression_function(
            identifier_span,
            FunctionType::FunctionExpression,
            Some(function_identifier_ref),
            false,
            false,
            false,
            None::<oxc_allocator::Box<'_, TSTypeParameterDeclaration<'_>>>,
            None::<oxc_allocator::Box<'_, TSThisParameter<'_>>>,
            params,
            None::<oxc_allocator::Box<'_, TSTypeAnnotation<'_>>>,
            Some(function_body),
        );

        let property_definition = self.ast.class_element_property_definition(
            identifier_span,
            PropertyDefinitionType::PropertyDefinition,
            self.ast.vec(),
            identifier_key,
            None::<oxc_allocator::Box<'_, TSTypeAnnotation<'_>>>,
            Some(function_expression),
            false,
            true,
            false,
            false,
            false,
            false,
            false,
            None,
        );
        property_definition
    }
}
