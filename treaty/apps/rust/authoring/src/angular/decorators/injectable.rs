use std::{any::Any, fmt};

use oxc_ast::ast::{ Argument, Decorator, Expression, ObjectPropertyKind, PropertyKey};
#[derive(Debug)]
pub enum InjectableProvider {
    Value(ValueSansProvider),
    Existing(ExistingSansProvider),
    StaticClass(StaticClassSansProvider),
    Constructor(ConstructorSansProvider),
    Factory(FactorySansProvider),
    Class(ClassSansProvider),
    None,
}
impl Default for InjectableProvider {
    fn default() -> Self {
        {
            InjectableProvider::None
        }
    }
}
#[derive(Debug)]
struct ValueSansProvider {
    use_value: Box<dyn Any>,
}
#[derive(Debug)]
struct ExistingSansProvider {
    use_existing: Box<dyn Any>,
}
#[derive(Debug)]
struct StaticClassSansProvider {
    use_class: Box<dyn Any>,
    deps: Vec<Box<dyn Any>>,
}
#[derive(Debug)]
struct ConstructorSansProvider {
    deps: Option<Vec<Box<dyn Any>>>,
}
struct FactorySansProvider {
    use_factory: Box<dyn Fn(Vec<Box<dyn Any>>) -> Box<dyn Any>>,
    deps: Option<Vec<Box<dyn Any>>>,
}
impl fmt::Debug for FactorySansProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FactorySansProvider")
            .field("use_factory", &"Box<dyn Fn(Vec<Box<dyn Any>>) -> Box<dyn Any>>")
            .field("deps", &self.deps)
            .finish()
    }
}
#[derive(Debug)]
struct ClassSansProvider {
    use_class: Box<dyn Any>,
}

#[derive(Debug, PartialEq)]
pub enum ProviderScope {
    Root,
    Platform,
    Any,
    None
}
impl ProviderScope {
    pub fn from_str(name: &str) -> Option<Self> {
        match name {
            "root" => Some(Self::Root),
            "platform" => Some(Self::Platform),
            "any" => Some(Self::Any),
            _ => None,
        }
    }
}
impl ToString for ProviderScope {
    fn to_string(&self) -> String {
        match self {
            ProviderScope::Root => String::from("root"),
            ProviderScope::Platform => String::from("platform"),
            ProviderScope::Any => String::from("any"),
            ProviderScope::None => String::from("none"),
        }
    }
}
#[derive(Debug)]
pub struct InjectableOptions {
    pub provided_in: ProviderScope,
    pub detail: InjectableProvider,
}
impl Default for InjectableOptions {
    fn default() -> Self {
        InjectableOptions {
            provided_in: ProviderScope::None,
            detail: InjectableProvider::None
        }
    }
}
impl InjectableOptions {
    pub const PROVIDED_IN_KEY: &'static str = "providedIn";

    pub fn parse_decorator(decorator: &Decorator) -> Option<InjectableOptions> {
        if let Expression::CallExpression(call_expr) = &decorator.expression {
            call_expr.arguments.iter().find_map(|arg| {
                if let Argument::Expression(Expression::ObjectExpression(obj_expr)) = arg {
                    InjectableOptions::from_properties(&obj_expr.properties)
                } else {
                    None
                }
            })
        } else {
            None
        }
    }

    pub fn from_properties(properties: &[ObjectPropertyKind]) -> Option<Self> {
        properties
            .iter()
            .filter_map(|property_kind| {
                if let ObjectPropertyKind::ObjectProperty(boxed_property) = property_kind {
                    match boxed_property.key {
                        PropertyKey::Identifier(ref identifier)
                        if identifier.name == Self::PROVIDED_IN_KEY =>
                        {
                            match &boxed_property.value {
                                Expression::StringLiteral(literal) => {
                                    ProviderScope::from_str(&literal.value)
                                        .map(|provided_in| Self { provided_in, detail: InjectableProvider::None })
                                }
                                _ => None,
                            }
                        }
                        _ => None,
                    }
                } else {
                    None
                }
            })
            .next()
    }

}
