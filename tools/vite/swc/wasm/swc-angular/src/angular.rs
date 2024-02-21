use std::collections::HashMap;
use std::any::Any;
use std::fmt::Debug;

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
    pub style_url: Option<String>,
    pub style_urls: Vec<String>,
    pub styles: Vec<String>,
    pub animations: Vec<Box<dyn Any>>, // Simplified representation
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



pub trait NgTraitHandler {
    fn parse(&self, node: &Expr) -> Result<NgTraitMeta, String>;
    fn transform_to_ivy(&self, meta: &NgTraitMeta) -> Result<NgTraitMeta, String>;
    fn transform_to_JIT(&self, meta: &NgTraitMeta) -> Result<NgTraitMeta, String>;
}


pub struct NgTraitHandlerFactory;

impl NgTraitHandlerFactory {
    pub fn get_handler(trait_type: &NgTrait) -> Box<dyn NgTraitHandler> {
        match trait_type {
            NgTrait::Component => Box::new(ComponentHandler),
            NgTrait::Directive => Box::new(DirectiveHandler),
            NgTrait::Injectable => Box::new(InjectableHandler),
            NgTrait::NgModule => Box::new(NgModuleHandler),
            NgTrait::Pipe => Box::new(PipeHandler),
        }
    }
}