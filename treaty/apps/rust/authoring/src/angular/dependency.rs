use oxc::syntax::operator::LogicalOperator;
use oxc_span::SPAN;
use std::rc::Rc;

use oxc_ast::{ast::*, AstBuilder, AstKind};

use super::ParamDecorator;

use super::TopLevelDecorator;

use super::{context::{AngularCtx, AngularContext}, InjectableOptions};

pub struct DependencyInjection<'a> {
    ast: Rc<AstBuilder<'a>>,
    nodes: oxc_allocator::Vec<'a, AstKind<'a>>,
    context: AngularCtx<'a>,
    constructor_params: Vec<ParamDecorator>,
}

impl<'a> DependencyInjection<'a> {
    pub fn new(ast: Rc<AstBuilder<'a>>, context: AngularCtx<'a>) -> Option<Self> {
        let nodes: oxc_allocator::Vec<'_, AstKind<'_>> = ast.new_vec();
        Some(Self {
            ast,
            nodes,
            context,
            constructor_params: Vec::new(),
        })
    }

    pub fn transform_class(&mut self, class: &mut Class<'a>) {
        let has_decorator = !class.decorators.is_empty();
        let class_name = if has_decorator {
            class
                .id
                .clone()
                .map(|id| self.context.scopes().generate_uid(&id.name))
                .or_else(|| Some(self.context.scopes().generate_uid("class")))
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
                    std::mem::replace(&mut constructor.value.params.items, self.ast.new_vec());

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
                                    ParamDecorator::Inject(ref _name)
                                    | ParamDecorator::ASelf(ref _name)
                                    | ParamDecorator::Optional(ref _name)
                                    | ParamDecorator::SkipSelf(ref _name) => {
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
                        if let Some(type_name) = self
                            .extract_type_name_from_type_annotation(&param.pattern.type_annotation.as_ref())
                        {
                            self.constructor_params
                                .push(ParamDecorator::Inject(type_name));
                        }
                    }
                }
                constructor.value.params.items = temp_params;
            }
        }

        let factory_name = format!("factory{}", class_name.unwrap_or_default());
        if has_decorator {
            let mut index = 0;
            while index < class.decorators.len() {
                let decorator = &class.decorators[index];
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
                    if let Some(TopLevelDecorator::Injectable { options }) = TopLevelDecorator::from_str(name, decorator) {
                        // Process the Injectable decorator
                        println!("Processing Injectable decorator with options: {:?}", options);
                        let property_definition = self.ng_factory_builder(&options, factory_name.clone());
                        class.body.body.insert(0, property_definition);
                        class.decorators.remove(index);
                        continue;
                    }
                }
               index += 1;
            }
        }
    }

    fn extract_type_name_from_type_annotation(
        &self,
        type_annotation:  &Option<&oxc_allocator::Box<oxc_ast::ast::TSTypeAnnotation>>
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
        }
    }

    fn ng_factory_builder(
        &self,
        injectable_options: &InjectableOptions,
        factory_name: String,
    ) -> ClassElement<'a> {
        let identifier_name = self.ast.new_atom("ɵfac");
        let identifier_span = SPAN;
        let identifier_key = self.ast.property_key_identifier(IdentifierName {
            span: identifier_span,
            name: identifier_name,
        });

        let function_identifier_name = self.ast.new_atom(&factory_name);
        let function_identifier_ref =
            BindingIdentifier::new(identifier_span, function_identifier_name.clone());
        let function_identifier =
            self.ast
                .identifier_reference_expression(IdentifierReference::new(
                    identifier_span,
                    function_identifier_name,
                ));

        let param_identifier_name = self.ast.new_atom("t");
        let param_identifier_expression =
            self.ast
                .identifier_reference_expression(IdentifierReference::new(
                    identifier_span,
                    param_identifier_name.clone(),
                ));

        let param_identifier = self.ast.alloc(BindingIdentifier::new(
            identifier_span,
            param_identifier_name,
        ));

        let param_binding_pattern =
            BindingPattern::new_with_kind(BindingPatternKind::BindingIdentifier(param_identifier));

        let formal_parameter = self.ast.formal_parameter(
            identifier_span,
            param_binding_pattern,
            None,
            false,
            false,
            self.ast.new_vec(),
        );

        let params: oxc_allocator::Box<'_, FormalParameters<'_>> = self.ast.formal_parameters(
            identifier_span,
            FormalParameterKind::FormalParameter,
            self.ast.new_vec_single(formal_parameter),
            None,
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
            .new_vec_with_capacity::<Argument<'a>>(inject_tokens.len());

        for token in inject_tokens {
            let token_identifier_name = self.ast.new_atom(&token);
            let token_identifier =
                self.ast
                    .identifier_reference_expression(IdentifierReference::new(
                        identifier_span,
                        token_identifier_name,
                    ));

            let i0_identifier_name = self.ast.new_atom("i0");
            let i0_identifier = self
                .ast
                .identifier_reference_expression(IdentifierReference::new(
                    identifier_span,
                    i0_identifier_name,
                ));

            let inject_identifier_name = self.ast.new_atom("ɵɵinject");

            // Create the call expression for each token
            let inject_call_expression = self.ast.call_expression(
                identifier_span,
                self.ast.static_member_expression(
                    identifier_span,
                    i0_identifier,
                    IdentifierName {
                        span: identifier_span,
                        name: inject_identifier_name.clone(),
                    },
                    false,
                ),
                self.ast
                    .new_vec_single(Argument::Expression(token_identifier)),
                false,
                None,
            );

            // Add the call expression as an argument to the new_expression_arguments
            new_expression_arguments.push(Argument::Expression(inject_call_expression));
        }

        // Creating the new expression with the aggregated arguments
        let new_expression = self.ast.new_expression(
            identifier_span,
            self.ast.parenthesized_expression(
                identifier_span,
                self.ast.logical_expression(
                    identifier_span,
                    param_identifier_expression,
                    LogicalOperator::Or,
                    function_identifier,
                ),
            ),
            new_expression_arguments, // Use the aggregated call expressions here
            None,
        );

        let return_statement = self
            .ast
            .return_statement(identifier_span, Some(new_expression));

        let function_body = self.ast.function_body(
            identifier_span,
            self.ast.new_vec(),
            self.ast.new_vec_single(return_statement),
        );

        let function_expression = self.ast.function_expression(self.ast.function(
            FunctionType::FunctionExpression,
            identifier_span,
            Some(function_identifier_ref),
            false,
            false,
            None,
            params,
            Some(function_body),
            None,
            None,
            Modifiers::empty(),
        ));

        let property_definition = self.ast.class_property(
            PropertyDefinitionType::PropertyDefinition,
            identifier_span,
            identifier_key,
            Some(function_expression),
            false,
            true,
            self.ast.new_vec(),
        );
        property_definition
    }
}
