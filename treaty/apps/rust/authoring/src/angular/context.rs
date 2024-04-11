use std::{
    cell::{Ref, RefCell, RefMut},
    mem,
    rc::Rc,
};

use oxc_ast::AstBuilder;
use oxc_diagnostics::Error;
use oxc_semantic::{ScopeId, ScopeTree, Semantic, SymbolId, SymbolTable};
use oxc_span::{CompactString, SourceType};

#[derive(Clone)]
pub struct AngularCtx<'a>(pub Rc<AstBuilder<'a>>, Rc<RefCell<Semantic<'a>>>, Rc<RefCell<Vec<Error>>>);

pub trait AngularContext<'a> {
    fn new(ast: Rc<AstBuilder<'a>>, semantic: Rc<RefCell<Semantic<'a>>>) -> Self;

    fn semantic(&self) -> Ref<'_, Semantic<'a>>;

    fn symbols(&self) -> Ref<'_, SymbolTable>;

    fn scopes(&self) -> Ref<'_, ScopeTree>;

    fn scopes_mut(&self) -> RefMut<'_, ScopeTree>;

    fn add_binding(&self, name: CompactString);

    fn source_type(&self) -> Ref<'_, SourceType>;

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

    fn symbols(&self) -> Ref<'_, SymbolTable> {
        Ref::map(self.1.borrow(), |semantic| semantic.symbols())
    }

    fn scopes(&self) -> Ref<'_, ScopeTree> {
        Ref::map(self.1.borrow(), |semantic| semantic.scopes())
    }

    fn scopes_mut(&self) -> RefMut<'_, ScopeTree> {
        RefMut::map(self.1.borrow_mut(), |semantic| semantic.scopes_mut())
    }

    fn add_binding(&self, name: CompactString) {
        // TODO: use the correct scope and symbol id
        self.scopes_mut().add_binding(ScopeId::new(0), name, SymbolId::new(0));
    }

    fn source_type(&self) -> Ref<'_, SourceType> {
        Ref::map(self.1.borrow(), |semantic| semantic.source_type())
    }

    fn errors(&self) -> Vec<Error> {
        mem::take(&mut self.2.borrow_mut())
    }

    /// Push a Transform Error
    fn error<T: Into<Error>>(&mut self, error: T) {
        self.2.borrow_mut().push(error.into());
    }
}
