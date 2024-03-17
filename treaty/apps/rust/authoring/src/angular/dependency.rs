use std::rc::Rc;
use oxc_allocator::Vec;

use oxc_ast::{AstBuilder, AstKind, VisitMut};


pub struct DependencyInjection<'a> {
    ast: Rc<AstBuilder<'a>>,
    nodes: Vec<'a, AstKind<'a>>,
}

impl<'a> DependencyInjection<'a> {
    pub fn new(
        ast: Rc<AstBuilder<'a>>,
    ) -> Option<Self> {
        let nodes = ast.new_vec();
        Some(Self { ast, nodes })
    }
}


impl<'a> VisitMut<'a> for DependencyInjection<'a> {
     fn enter_node(&mut self, kind: AstKind<'a>) {
        self.nodes.push(kind);
    }

    fn leave_node(&mut self, _kind: AstKind<'a>) {
        self.nodes.pop();
    }
}
