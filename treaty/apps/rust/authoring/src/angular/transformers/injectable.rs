use crate::angular::{context::AngularContext, InjectableOptions};
use crate::angular::{ProviderScope, TopLevelDecorator};

use std::rc::Rc;

use oxc::syntax::class;
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
        let nodes: oxc_allocator::Vec<'_, AstKind<'_>> = ast.new_vec();
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
                .map(|id| id.name.to_compact_string())
                .or_else(|| Some(self.context.scopes().generate_uid("class")))
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
                    class.body.body.insert(0, property_definition);
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
        let static_property_key = self.ast.property_key_identifier(IdentifierName {
            span: SPAN,
            name: self.ast.new_atom("ɵprov"),
        });

        let i0_identifier_name = self.ast.new_atom("i0");
        let i0_identifier = self
            .ast
            .identifier_reference_expression(IdentifierReference::new(SPAN, i0_identifier_name));

        // Create a new vector for properties
        let mut properties = self.ast.new_vec();

        properties.push(ObjectPropertyKind::ObjectProperty(
            self.ast.object_property(
                SPAN,
                PropertyKind::Init,
                self.ast
                    .property_key_identifier(IdentifierName::new(SPAN, "token".into())),
                self.ast
                    .identifier_reference_expression(IdentifierReference::new(
                        SPAN,
                        self.ast.new_atom(&class_name),
                    )),
                None,
                false,
                false,
                false,
            ),
        ));

        properties.push(ObjectPropertyKind::ObjectProperty(
            self.ast.object_property(
                SPAN,
                PropertyKind::Init,
                self.ast
                    .property_key_identifier(IdentifierName::new(SPAN, "factory".into())),
                self.ast.static_member_expression(
                    SPAN,
                    self.ast
                        .identifier_reference_expression(IdentifierReference::new(
                            SPAN,
                            class_name.into(),
                        )),
                    IdentifierName {
                        span: SPAN,
                        name: "ɵfac".into(),
                    },
                    false,
                ),
                None,
                false,
                false,
                false,
            ),
        ));

        if injectable_options.provided_in != ProviderScope::None {
            properties.push(ObjectPropertyKind::ObjectProperty(
                self.ast.object_property(
                    SPAN,
                    PropertyKind::Init,
                    self.ast
                        .property_key_identifier(IdentifierName::new(SPAN, "providedIn".into())),
                    self.ast.literal_string_expression(StringLiteral::new(
                        SPAN,
                        self.ast
                            .new_atom(&injectable_options.provided_in.to_string()),
                    )),
                    None,
                    false,
                    false,
                    false,
                ),
            ));
        }

        let define_injectable_object = self.ast.object_expression(SPAN, properties, None);

        let define_injectable_name = self.ast.new_atom("ɵɵdefineInjectable");
        let define_injectable_call_expression = self.ast.call_expression(
            SPAN,
            self.ast.static_member_expression(
                SPAN,
                i0_identifier,
                IdentifierName {
                    span: SPAN,
                    name: define_injectable_name.clone(),
                },
                false,
            ),
            self.ast
                .new_vec_single(Argument::Expression(define_injectable_object)),
            false,
            None,
        );

        let property_definition = self.ast.class_property(
            PropertyDefinitionType::PropertyDefinition,
            SPAN,
            static_property_key,
            Some(define_injectable_call_expression),
            false,
            true,
            self.ast.new_vec(),
        );
        property_definition
    }
}
