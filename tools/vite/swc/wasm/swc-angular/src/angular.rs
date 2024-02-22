use std::collections::HashMap;
use std::any::Any;
use std::fmt::Debug;
use swc_core::ecma::ast::{ExprOrSpread};
use swc_ecma_ast::Class;
use crate::classes::{
    component::ComponentHandler,
    directive::DirectiveHandler, 
    injectable::InjectableHandler,
    module::NgModuleHandler,
    pipe::PipeHandler
};

pub enum ChangeDetectionStrategy {
    OnPush,
    Default,
}

pub enum ViewEncapsulation {
    Emulated,
    None,
    ShadowDom,
}


pub enum NgTrait {
    Component,
    Directive,
    Injectable,
    NgModule,
    Pipe,
}



pub enum NgTraitMeta {
    Directive(DirectiveMeta),
    Component(ComponentMeta),
    Injectable(InjectableMeta),
    Pipe(PipeMeta),
    NgModule(ModuleMeta),
}

pub struct PipeMeta {
    pub standalone: StandaloneMeta,
    pub name: String,
    pub pure: Option<bool>,
}

pub struct StandaloneMeta {
    pub standalone: Option<bool>,
}
impl StandaloneMeta {
    pub fn new(standalone: Option<bool>) -> Self {
        Self { standalone }
    }
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputFlags {
    None = 0,
    SignalBased = 1,
    HasDecoratorInputTransform = 2,
}

impl InputFlags {
    pub fn from_i32(value: i32) -> Option<Self> {
        match value {
            0 => Some(InputFlags::None),
            1 => Some(InputFlags::SignalBased),
            2 => Some(InputFlags::HasDecoratorInputTransform),
            _ => None,
        }
    }

    pub fn to_i32(self) -> i32 {
        self as i32
    }
}

#[derive(Debug, Clone)]
struct InputTransformFunction; 

#[derive(Debug, Clone)]
enum DirectiveInputValue {
    Simple(String),
    Detailed {
        flags: InputFlags,
        public_name: String,
        declared_name: Option<String>,
        transform: Option<InputTransformFunction>, 
    },
}


#[derive(Debug, Clone)]
enum HostDirective {
    Type(String),
    Detailed {
        directive: String, 
        inputs: Option<Vec<String>>,
        outputs: Option<Vec<String>>,
    },
}
pub struct ModuleMeta {
    pub imports: Vec<Box<dyn Any>>,
    pub declare: Vec<Box<dyn Any>>,
}

pub struct DirectiveMeta {
    pub selector: String,
    pub inputs: HashMap<String, DirectiveInputValue>,
    pub outputs: Vec<String>,
    pub providers: Vec<Box<dyn Any>>,
    pub export_as: Option<String>,
    pub queries: HashMap<String, Box<dyn Any>>,
    pub host: HashMap<String, String>,
    pub jit: Option<bool>,
    pub standalone: StandaloneMeta,
    pub host_directives: Vec<HostDirective>,
}
pub struct ComponentMeta {
    pub directive: DirectiveMeta,
    pub template_url: Option<String>,
    pub template: Option<String>,
    pub style_urls: Vec<String>,
    pub styles: Vec<String>,
    pub animations: Vec<Box<dyn Any>>,
    pub encapsulation: Option<ViewEncapsulation>,
    pub interpolation: Option<(String, String)>,
    pub preserve_whitespaces: Option<bool>,
    pub standalone: Option<bool>,
    pub imports: Vec<Box<dyn Any>>,
}

pub enum ProvidedIn {
    Type(String),
    Root,
    Platform,
    Any,
    Null,
}

pub struct InjectableMeta {
    pub provided_in: ProvidedIn,
}



pub trait NgTraitHandler<M> {
    fn parse(&self, node: &Vec<ExprOrSpread>) -> Result<M, String>;
    fn transform_to_ivy(&self, meta: &M, class: &mut Class) -> Result<(), String>;
    fn transform_to_jit(&self, meta: &M, class: &mut Class) -> Result<(), String>;
}


#[derive()]
pub enum AnyNgTraitHandler {
    Component(ComponentHandler),
    Directive(DirectiveHandler),
    Injectable(InjectableHandler),
    NgModule(NgModuleHandler),
    Pipe(PipeHandler),
}

impl AnyNgTraitHandler {
    pub fn parse(&self, node: &Vec<ExprOrSpread>) -> Result<NgTraitMeta, String> {
        match self {
            AnyNgTraitHandler::Component(handler) => {
                handler.parse(node).map(NgTraitMeta::Component).map_err(|e| e.to_string())
            },
            AnyNgTraitHandler::Directive(handler) => {
                handler.parse(node).map(NgTraitMeta::Directive).map_err(|e| e.to_string())
            },
            AnyNgTraitHandler::Injectable(handler) => {
                handler.parse(node).map(NgTraitMeta::Injectable).map_err(|e| e.to_string())
            },
            AnyNgTraitHandler::Pipe(handler) => {
                handler.parse(node).map(NgTraitMeta::Pipe).map_err(|e| e.to_string())
            },
            AnyNgTraitHandler::NgModule(handler) => {
                handler.parse(node).map(NgTraitMeta::NgModule).map_err(|e| e.to_string())
            },
        }
    }

    pub fn transform_to_ivy(&self, meta: &NgTraitMeta, class: &mut Class) -> Result<(), String> {
        match self {
            AnyNgTraitHandler::Component(handler) => {
                if let NgTraitMeta::Component(c_meta) = meta {
                    handler.transform_to_ivy(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::Directive(handler) => {
                if let NgTraitMeta::Directive(c_meta) = meta {
                    handler.transform_to_ivy(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::Injectable(handler) => {
                if let NgTraitMeta::Injectable(c_meta) = meta {
                    handler.transform_to_ivy(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::NgModule(handler) => {
                if let NgTraitMeta::NgModule(c_meta) = meta {
                    handler.transform_to_ivy(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::Pipe(handler) => {
                if let NgTraitMeta::Pipe(c_meta) = meta {
                    handler.transform_to_ivy(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
        }
    }

    pub fn transform_to_jit(&self, meta: &NgTraitMeta, class: &mut Class) -> Result<(), String> {
        match self {
            AnyNgTraitHandler::Component(handler) => {
                if let NgTraitMeta::Component(c_meta) = meta {
                    handler.transform_to_jit(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::Directive(handler) => {
                if let NgTraitMeta::Directive(c_meta) = meta {
                    handler.transform_to_jit(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::Injectable(handler) => {
                if let NgTraitMeta::Injectable(c_meta) = meta {
                    handler.transform_to_jit(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::NgModule(handler) => {
                if let NgTraitMeta::NgModule(c_meta) = meta {
                    handler.transform_to_jit(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
            AnyNgTraitHandler::Pipe(handler) => {
                if let NgTraitMeta::Pipe(c_meta) = meta {
                    handler.transform_to_jit(c_meta, class)
                } else {
                    Err("Mismatched meta type for ComponentHandler".to_string())
                }
            },
        }
    }
}

pub struct NgTraitHandlerFactory;

impl NgTraitHandlerFactory {
    pub fn get_handler(trait_type: &NgTrait) -> AnyNgTraitHandler {
        match trait_type {
            NgTrait::Component => AnyNgTraitHandler::Component(ComponentHandler),
            NgTrait::Directive => AnyNgTraitHandler::Directive(DirectiveHandler),
            NgTrait::Injectable => AnyNgTraitHandler::Injectable(InjectableHandler),
            NgTrait::NgModule => AnyNgTraitHandler::NgModule(NgModuleHandler),
            NgTrait::Pipe => AnyNgTraitHandler::Pipe(PipeHandler),
        }
    }
}