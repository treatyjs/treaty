use std::{
    cell::{Ref, RefCell},
    mem,
    rc::Rc,
};

use oxc_ast::AstBuilder;
use oxc_diagnostics::Error;
use oxc_semantic::Semantic;

#[derive(Clone)]
pub struct AngularCtx<'a>(pub Rc<AstBuilder<'a>>, Rc<RefCell<Semantic<'a>>>, Rc<RefCell<Vec<Error>>>);

pub trait AngularContext<'a> {
    fn new(ast: Rc<AstBuilder<'a>>, semantic: Rc<RefCell<Semantic<'a>>>) -> Self;

    fn semantic(&self) -> Ref<'_, Semantic<'a>>;

    /// Mint a (currently non-uniquified) identifier name. In OXC 0.133 the real
    /// `generate_uid` moved to `oxc_traverse::TraverseCtx`; this shim is sufficient
    /// for the legacy DI codegen and is superseded by the render3 factory port.
    fn generate_uid(&self, name: &str) -> String;

    fn errors(&self) -> Vec<Error>;

    /// Push a Transform Error
    fn error<T: Into<Error>>(&mut self, error: T);
}

impl<'a> AngularContext<'a> for AngularCtx<'a> {
    fn new(ast: Rc<AstBuilder<'a>>, semantic: Rc<RefCell<Semantic<'a>>>) -> Self {
        Self(ast, semantic, Rc::new(RefCell::new(vec![])))
    }

    fn semantic(&self) -> Ref<'_, Semantic<'a>> {
        self.1.borrow()
    }

    fn generate_uid(&self, name: &str) -> String {
        name.to_string()
    }

    fn errors(&self) -> Vec<Error> {
        mem::take(&mut self.2.borrow_mut())
    }

    /// Push a Transform Error
    fn error<T: Into<Error>>(&mut self, error: T) {
        self.2.borrow_mut().push(error.into());
    }
}
