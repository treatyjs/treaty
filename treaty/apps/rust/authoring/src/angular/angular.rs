#![allow(clippy::wildcard_imports, clippy::option_map_unit_fn)]


use std::{cell::RefCell, rc::Rc, sync::Arc};

use oxc_allocator::Allocator;
use oxc_ast::{ast::*, AstBuilder, AstKind, VisitMut};
use oxc_diagnostics::Error;
use oxc_semantic::Semantic;
use oxc_span::SourceType;
use super::context::{AngularCtx, AngularContext};
use super::transformers::DependencyInjection;
use super::TopLevelDecorator;


#[derive()]
pub struct Angular<'a> {
    ctx: AngularCtx<'a>,
    dependency_injection: Option<DependencyInjection<'a>>,

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
}

impl<'a> VisitMut<'a> for Angular<'a> {
    fn visit_class_body(&mut self, class_body: &mut ClassBody<'a>) {

        class_body.body.iter_mut().for_each(|class_element| {
            self.visit_class_element(class_element);
        });
    }

    fn visit_class(&mut self, class: &mut Class<'a>) {
        let mut top_level_decorators: Vec<(TopLevelDecorator, usize)> = Vec::new();
        for (index, decorator) in class.decorators.iter().enumerate() {
            if let Expression::CallExpression(boxed_expr) = &decorator.expression {
                if let Expression::Identifier(identifier) = &boxed_expr.callee {
                    if let Some(top_level_decorator) = TopLevelDecorator::from_str(&identifier.name, decorator) {
                        top_level_decorators.push((top_level_decorator, index));
                    }
                }
            }
        }



        self.dependency_injection.as_mut().map(|t: &mut DependencyInjection<'_>| t.transform_class(class, top_level_decorators));
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
