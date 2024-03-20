use std::rc::Rc;

use oxc_ast::ast::{
    Argument, CallExpression, ClassBody, Decorator, Expression, FormalParameter, ObjectPropertyKind, PropertyKey
};

use super::injectable::InjectableOptions;
#[derive(Debug)]
pub enum TopLevelDecorator {
    Component,
    Directive,
    Pipe,
    NgModule,
    Injectable { options: InjectableOptions },
}

impl TopLevelDecorator {
    pub fn from_str(name: &str, decorator: &Decorator) -> Option<Self> {
        match name {
            "Component" => Some(Self::Component),
            "Directive" => Some(Self::Directive),
            "Pipe" => Some(Self::Pipe),
            "NgModule" => Some(Self::NgModule),
            "Injectable" => {
                let options = InjectableOptions::parse_decorator(decorator).unwrap_or_default();
                Some(Self::Injectable { options })
            }
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParamDecorator {
    Optional(String),
    ASelf(String),
    SkipSelf(String),
    Inject(String),
    None,
}

impl ParamDecorator {
    pub fn from_str(name: &str, param: &FormalParameter, decorator: &Decorator) -> Option<Self> {
        let _ = param;
        // println!("{:#?}", param);
        match name {
            "Optional" => Some(ParamDecorator::Optional("".into())),
            "Self" => Some(ParamDecorator::ASelf("".into())),
            "SkipSelf" => Some(ParamDecorator::SkipSelf("".into())),
            "Inject" => {
                if let Expression::CallExpression(call_expr) = &decorator.expression {
                    if let Some(Argument::Expression(Expression::Identifier(identifier_reference))) = call_expr.arguments.get(0) {
                        Some(ParamDecorator::Inject(identifier_reference.name.to_string()))
                    } else {
                        None
                    }
                } else {
                    None
                }

            },
            _ => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PropertyDecorator {
    Input,
    Output,
    HostBinding,
    HostListener,
    ViewChild,
    ViewChildren,
    ContentChild,
    ContentChildren,
    Attribute,
}
impl PropertyDecorator {
    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "Input" => Some(Self::Input),
            "Output" => Some(Self::Output),
            "HostBinding" => Some(Self::HostBinding),
            "HostListener" => Some(Self::HostListener),
            "ViewChild" => Some(Self::ViewChild),
            "ViewChildren" => Some(Self::ViewChildren),
            "ContentChild" => Some(Self::ContentChild),
            "ContentChildren" => Some(Self::ContentChildren),
            "Attribute" => Some(Self::Attribute),
            _ => None,
        }
    }
}
