use std::collections::HashMap;

use lazy_static::lazy_static;


#[derive(Copy, Clone, Hash, Eq, PartialEq)]
pub enum Identifier {
    Advance,
    DefineComponent,
    Element,
    ElementEnd,
    ElementStart,

    Template,
    Text,
    TextInterpolate,
    TextInterpolate1,
    TextInterpolate2,
    TextInterpolate3,
    TextInterpolate4,
    TextInterpolate5,
    TextInterpolate6,
    TextInterpolate7,
    TextInterpolate8,
    TextInterpolateV,
}

impl Identifier {
    pub fn imported<OpaqueT>(self) -> (String, String) {
        return (
            "@angular/core".to_string(),
            RUNTIME.get(&self).unwrap().to_string(),
        );
    }
}

lazy_static! {
    static ref RUNTIME: HashMap<Identifier, &'static str> = {
        HashMap::from([
            (Identifier::Advance, "ɵɵadvance"),
            (Identifier::DefineComponent, "ɵɵdefineComponent"),
            (Identifier::Element, "ɵɵelement"),
            (Identifier::ElementEnd, "ɵɵelementEnd"),
            (Identifier::ElementStart, "ɵɵelementStart"),
            (Identifier::Template, "ɵɵtemplate"),
            (Identifier::Text, "ɵɵtext"),
            (Identifier::TextInterpolate, "ɵɵtextInterpolate"),
            (Identifier::TextInterpolate1, "ɵɵtextInterpolate1"),
            (Identifier::TextInterpolate2, "ɵɵtextInterpolate2"),
            (Identifier::TextInterpolate3, "ɵɵtextInterpolate3"),
            (Identifier::TextInterpolate4, "ɵɵtextInterpolate4"),
            (Identifier::TextInterpolate5, "ɵɵtextInterpolate5"),
            (Identifier::TextInterpolate6, "ɵɵtextInterpolate6"),
            (Identifier::TextInterpolate7, "ɵɵtextInterpolate7"),
            (Identifier::TextInterpolate8, "ɵɵtextInterpolate8"),
            (Identifier::TextInterpolateV, "ɵɵtextInterpolateV"),
        ])
    };
}
