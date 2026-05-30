use crate::angular::{context::AngularContext, InjectableOptions};
use crate::angular::{ProviderScope, TopLevelDecorator};

use std::rc::Rc;

use oxc_ast::{ast::*, AstBuilder, AstKind};
use oxc_span::SPAN;

use crate::angular::context::AngularCtx;

pub struct InjectableCreator<'a> {
    ast: Rc<AstBuilder<'a>>,
    nodes: oxc_allocator::Vec<'a, AstKind<'a>>,
    context: AngularCtx<'a>,
}

impl<'a> InjectableCreator<'a> {
    pub fn new(ast: Rc<AstBuilder<'a>>, context: AngularCtx<'a>) -> Option<Self> {
        let nodes: oxc_allocator::Vec<'_, AstKind<'_>> = ast.vec();
        Some(Self {
            ast,
            nodes,
            context,
        })
    }

    pub fn specifier_to_remove() -> Vec<&'a str> {
        vec!["Injectable"]
    }

    pub fn transform_class(
        &mut self,
        class: &mut Class<'a>,
        top_level_decorators: &Vec<(TopLevelDecorator, usize)>,
    ) {
        let class_name = if top_level_decorators.len() > 0 {
            class
                .id
                .clone()
                .map(|id| id.name.as_str().to_string())
                .or_else(|| Some(self.context.generate_uid("class")))
        } else {
            None
        };
        for (decorator, index) in top_level_decorators {
            match decorator {
                TopLevelDecorator::Injectable { options } => {
                    class.decorators.remove(*index);
                    let property_definition = self.ng_factory_builder(
                        class_name.clone().unwrap_or_default().to_string(),
                        &options,
                    );
                    class.body.body.insert(1, property_definition);
                }
                _ => (),
            }
        }
    }

    fn ng_factory_builder(
        &self,
        class_name: String,
        injectable_options: &InjectableOptions,
    ) -> ClassElement<'a> {
        let static_property_key = self.ast.property_key_static_identifier(SPAN, "ɵprov");

        let i0_identifier = self.ast.expression_identifier(SPAN, "i0");

        // Create a new vector for properties
        let mut properties = self.ast.vec();

        properties.push(ObjectPropertyKind::ObjectProperty(self.ast.alloc(
            self.ast.object_property(
                SPAN,
                PropertyKind::Init,
                self.ast.property_key_static_identifier(SPAN, "token"),
                self.ast
                    .expression_identifier(SPAN, self.ast.ident(&class_name)),
                false,
                false,
                false,
            ),
        )));

        properties.push(ObjectPropertyKind::ObjectProperty(self.ast.alloc(
            self.ast.object_property(
                SPAN,
                PropertyKind::Init,
                self.ast.property_key_static_identifier(SPAN, "factory"),
                Expression::StaticMemberExpression(self.ast.alloc(
                    self.ast.static_member_expression(
                        SPAN,
                        self.ast
                            .expression_identifier(SPAN, self.ast.ident(&class_name)),
                        self.ast.identifier_name(SPAN, "ɵfac"),
                        false,
                    ),
                )),
                false,
                false,
                false,
            ),
        )));

        if injectable_options.provided_in != ProviderScope::None {
            properties.push(ObjectPropertyKind::ObjectProperty(self.ast.alloc(
                self.ast.object_property(
                    SPAN,
                    PropertyKind::Init,
                    self.ast.property_key_static_identifier(SPAN, "providedIn"),
                    self.ast.expression_string_literal(
                        SPAN,
                        self.ast.str(&injectable_options.provided_in.to_string()),
                        None,
                    ),
                    false,
                    false,
                    false,
                ),
            )));
        }

        let define_injectable_object =
            Expression::ObjectExpression(self.ast.alloc(self.ast.object_expression(SPAN, properties)));

        let define_injectable_call_expression = self.ast.expression_call(
            SPAN,
            Expression::StaticMemberExpression(self.ast.alloc(self.ast.static_member_expression(
                SPAN,
                i0_identifier,
                self.ast.identifier_name(SPAN, "ɵɵdefineInjectable"),
                false,
            ))),
            Option::<TSTypeParameterInstantiation>::None,
            self.ast.vec1(Argument::from(define_injectable_object)),
            false,
        );

        self.ast.class_element_property_definition(
            SPAN,
            PropertyDefinitionType::PropertyDefinition,
            self.ast.vec(),
            static_property_key,
            Option::<TSTypeAnnotation>::None,
            Some(define_injectable_call_expression),
            false,
            true,
            false,
            false,
            false,
            false,
            false,
            None,
        )
    }
}
