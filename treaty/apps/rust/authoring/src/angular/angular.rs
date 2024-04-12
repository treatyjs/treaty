#![allow(clippy::wildcard_imports, clippy::option_map_unit_fn)]

use std::{cell::RefCell, rc::Rc, sync::Arc};

use super::context::{AngularContext, AngularCtx};
use super::transformers::DependencyInjection;
use super::{InjectableCreator, TopLevelDecorator};
use oxc_allocator::Allocator;
use oxc_ast::{ast::*, AstBuilder, AstKind, VisitMut};
use oxc_diagnostics::Error;
use oxc_semantic::Semantic;
use oxc_span::SourceType;

#[derive()]
pub struct Angular<'a> {
    ctx: AngularCtx<'a>,
    dependency_injection: Option<DependencyInjection<'a>>,
    injectable_creator: Option<InjectableCreator<'a>>,
}

impl<'a> Angular<'a> {
    #[rustfmt::skip]
    pub fn new(
        allocator: &'a Allocator,
        _source_type: SourceType,
        semantic: Semantic<'a>,
    ) -> Self {
        let ast = Rc::new(AstBuilder::new(allocator));
        let ctx = AngularCtx::new(
            Rc::clone(&ast),
            Rc::new(RefCell::new(semantic)),
        );
        Self {
            ctx: ctx.clone(),
            dependency_injection: DependencyInjection::new(Rc::clone(&ast), ctx.clone()),
            injectable_creator: InjectableCreator::new(Rc::clone(&ast), ctx.clone()),
        }
    }

    pub fn build(mut self, program: &mut Program<'a>) -> Result<(), Vec<Error>> {
        self.visit_program(program);
        let errors: Vec<_> = self
            .ctx
            .errors()
            .into_iter()
            .map(|e| e.with_source_code(Arc::new(self.ctx.semantic().source_text().to_string())))
            .collect();

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn remove_import_specifiers_matching_imported_name(
        specifiers: &mut Option<oxc_allocator::Vec<'a, ImportDeclarationSpecifier<'a>>>,
        desired_imported_names: Vec<&str>,
    ) {
        if let Some(specs) = specifiers.as_mut() {
            specs.retain(|specifier| {
                if let ImportDeclarationSpecifier::ImportSpecifier(import_spec) = specifier {
                    let imported_name_str: &str = &import_spec.imported.name();
                    !desired_imported_names.contains(&imported_name_str)
                } else {
                    true
                }
            });
        }
    }
}

impl<'a> VisitMut<'a> for Angular<'a> {
    fn visit_class_body(&mut self, class_body: &mut ClassBody<'a>) {
        class_body.body.iter_mut().for_each(|class_element| {
            self.visit_class_element(class_element);
        });
    }

    fn visit_class(&mut self, class: &mut Class<'a>) {
    //TODO: Strip this into context?
    let mut top_level_decorators: Vec<(TopLevelDecorator, usize)> = Vec::new();
    for (index, decorator) in class.decorators.iter().enumerate() {
        if let Expression::CallExpression(boxed_expr) = &decorator.expression {
            if let Expression::Identifier(identifier) = &boxed_expr.callee {
                if let Some(top_level_decorator) =
                    TopLevelDecorator::from_str(&identifier.name, decorator)
                {
                    top_level_decorators.push((top_level_decorator, index));
                }
            }
        }
    }

    self.dependency_injection
        .as_mut()
        .map(|t: &mut DependencyInjection<'_>| t.transform_class(class, &top_level_decorators));

    self.injectable_creator
        .as_mut()
        .map(|t: &mut InjectableCreator<'_>| t.transform_class(class, &top_level_decorators));
    }


    fn visit_import_declaration(&mut self, decl: &mut ImportDeclaration<'a>) {
        let kind = AstKind::ImportDeclaration(self.alloc(decl));

        let is_angular = decl
            .source
            .value
            .to_compact_string()
            .starts_with("@angular");
        self.enter_node(kind);

        if is_angular {
            let mut all_specifiers_to_remove: Vec<&str> = Vec::new();
            all_specifiers_to_remove.extend(DependencyInjection::specifier_to_remove());
            all_specifiers_to_remove.extend(InjectableCreator::specifier_to_remove());
            Self::remove_import_specifiers_matching_imported_name(
                &mut decl.specifiers,
                all_specifiers_to_remove
            );
        }
        if let Some(specifiers) = &mut decl.specifiers {
            for specifier in specifiers.iter_mut() {
                self.visit_import_declaration_specifier(specifier);
            }
        }
        self.visit_string_literal(&mut decl.source);
        self.leave_node(kind);
    }

    fn visit_statements(&mut self, stmts: &mut oxc_allocator::Vec<'a, Statement<'a>>) {
        for stmt in stmts.iter_mut() {
            self.visit_statement(stmt);
        }
        // self.dependency_injection.as_mut().map(|t| t.transform_statements(stmts));
    }

    fn visit_expression(&mut self, expr: &mut Expression<'a>) {
        // self.dependency_injection.as_mut().map(|t| t.transform_expression(expr));

        self.visit_expression_match(expr);
    }

    fn visit_decorator(&mut self, decorator: &mut Decorator<'a>) {
        let kind = AstKind::Decorator(self.alloc(decorator));
        // self.dependency_injection.as_mut().map(|t| t.transform_decorator(decorator));

        self.enter_node(kind);
        self.visit_expression(&mut decorator.expression);
        self.leave_node(kind);
    }
}
