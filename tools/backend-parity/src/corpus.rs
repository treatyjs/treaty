//! The shared backend-parity **corpus**.
//!
//! These fixtures are the `(id, template, selector, className)` tuples ported VERBATIM from
//! `libs/treaty-ivy/facade/parity/parity.mjs` (the `FIXTURES` array — the source of truth). That
//! oracle harness feeds the same tuples to Angular's `@angular/compiler` and to Treaty to prove
//! *Angular* parity; reusing the identical inputs here means the backend-parity gate (oxc vs swc,
//! per SWC-BACKEND-PLAN.md §4.1) rides on the corpus that already proves Angular parity and gets
//! backend parity "for free".
//!
//! Keep this list in sync with `parity.mjs`: when a fixture is added there, mirror it here. The
//! corpus is deliberately a hardcoded Rust constant (NOT parsed from the `.mjs` at runtime) so the
//! harness is dependency-free, deterministic, and compiles without a Node toolchain.

/// One corpus fixture. `selector` / `class_name` are fed identically to every backend so only
/// template lowering (parse) + printing (emit) can differ between backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fixture {
    /// Stable identifier (matches the `id` field in `parity.mjs`'s `FIXTURES`).
    pub id: &'static str,
    /// The component template HTML.
    pub template: &'static str,
    /// The component selector.
    pub selector: &'static str,
    /// The component class name.
    pub class_name: &'static str,
}

/// The full corpus, ported from `libs/treaty-ivy/facade/parity/parity.mjs` `FIXTURES`.
///
/// Ordered exactly as in `parity.mjs` so a side-by-side review against the source of truth is a
/// straight line-up. Reports iterate this slice in order, but artifacts are keyed by `id` in a
/// `BTreeMap` so on-disk ordering is stable regardless of this slice's order.
pub const CORPUS: &[Fixture] = &[
    // --- batch 1: element / interpolation / attribute basics --------------------------------
    Fixture { id: "static-element", template: "<button>Hi</button>", selector: "app-btn", class_name: "BtnComponent" },
    Fixture { id: "interpolation", template: "<div>{{name}}</div>", selector: "app-hello", class_name: "HelloComponent" },
    Fixture { id: "nested", template: "<div><span>{{name}}</span></div>", selector: "app-nested", class_name: "NestedComponent" },
    Fixture { id: "attribute", template: "<div class=\"box\" id=\"x\">y</div>", selector: "app-attr", class_name: "AttrComponent" },
    Fixture { id: "multiple-bindings", template: "<p>{{a}} and {{b}}</p>", selector: "app-multi", class_name: "MultiComponent" },
    // --- batch 2: bindings, events, control-flow, mixes -------------------------------------
    Fixture { id: "property-binding", template: "<div [id]=\"x\"></div>", selector: "app-prop", class_name: "PropComponent" },
    Fixture { id: "event-binding", template: "<button (click)=\"f()\">go</button>", selector: "app-event", class_name: "EventComponent" },
    Fixture { id: "two-interpolations", template: "<p>{{a}} {{b}}</p>", selector: "app-two-interp", class_name: "TwoInterpComponent" },
    Fixture { id: "class-binding", template: "<div [class.on]=\"b\">y</div>", selector: "app-class", class_name: "ClassComponent" },
    Fixture { id: "style-binding", template: "<div [style.color]=\"c\">y</div>", selector: "app-style", class_name: "StyleComponent" },
    Fixture { id: "control-flow-if", template: "<div>@if (cond) { <span>a</span> }</div>", selector: "app-if", class_name: "IfComponent" },
    Fixture { id: "control-flow-for", template: "<ul>@for (x of xs; track x) { <li>{{x}}</li> }</ul>", selector: "app-for", class_name: "ForComponent" },
    Fixture { id: "deep-nesting", template: "<div><p><span>{{x}}</span></p></div>", selector: "app-deep", class_name: "DeepComponent" },
    Fixture { id: "sibling-elements", template: "<div></div><span></span>", selector: "app-siblings", class_name: "SiblingsComponent" },
    Fixture { id: "static-and-bound-mix", template: "<div class=\"box\" [id]=\"x\">{{t}}</div>", selector: "app-mix", class_name: "MixComponent" },
    // --- batch 3: control-flow chains / branches and richer binding combos ------------------
    Fixture {
        id: "control-flow-if-elseif-else",
        template: "<div>@if (a) { <span>x</span> } @else if (b) { <span>y</span> } @else { <span>z</span> }</div>",
        selector: "app-if-chain",
        class_name: "IfChainComponent",
    },
    Fixture {
        id: "control-flow-switch",
        template: "<div>@switch (k) { @case (1) { <span>one</span> } @case (2) { <span>two</span> } @default { <span>other</span> } }</div>",
        selector: "app-switch",
        class_name: "SwitchComponent",
    },
    Fixture {
        id: "nested-for-in-if",
        template: "<div>@if (cond) { <ul>@for (x of xs; track x) { <li>{{x}}</li> }</ul> }</div>",
        selector: "app-nested-for-if",
        class_name: "NestedForIfComponent",
    },
    Fixture {
        id: "multiple-event-bindings",
        template: "<button (click)=\"f()\" (mouseenter)=\"g()\">go</button>",
        selector: "app-multi-event",
        class_name: "MultiEventComponent",
    },
    Fixture {
        id: "mixed-bindings-element",
        template: "<input [value]=\"v\" (input)=\"o($event)\" [class.err]=\"e\">",
        selector: "app-mixed-bind",
        class_name: "MixedBindComponent",
    },
    Fixture {
        id: "for-index-count",
        template: "<ul>@for (x of xs; track x) { <li>{{ $index }} of {{ $count }}: {{x}}</li> }</ul>",
        selector: "app-for-index",
        class_name: "ForIndexComponent",
    },
    Fixture {
        id: "ng-template-ref",
        template: "<ng-template #tpl><span>tpl</span></ng-template>",
        selector: "app-tpl-ref",
        class_name: "TplRefComponent",
    },
    Fixture { id: "attr-binding", template: "<div [attr.role]=\"r\"></div>", selector: "app-attr-bind", class_name: "AttrBindComponent" },
    // --- batch 4: pipes ---------------------------------------------------------------------
    Fixture { id: "pipe-simple", template: "<p>{{ x | uppercase }}</p>", selector: "app-pipe-simple", class_name: "PipeSimpleComponent" },
    Fixture { id: "pipe-with-args", template: "<p>{{ x | slice:1:3 }}</p>", selector: "app-pipe-args", class_name: "PipeArgsComponent" },
    Fixture { id: "pipe-chained", template: "<p>{{ x | uppercase | lowercase }}</p>", selector: "app-pipe-chained", class_name: "PipeChainedComponent" },
    Fixture { id: "pipe-in-binding", template: "<div [title]=\"t | uppercase\"></div>", selector: "app-pipe-binding", class_name: "PipeBindingComponent" },
    // --- batch 5: i18n ----------------------------------------------------------------------
    Fixture { id: "i18n-static", template: "<div i18n>Hello</div>", selector: "app-i18n-static", class_name: "I18nStaticComponent" },
    Fixture { id: "i18n-interp", template: "<div i18n>Hello {{name}}</div>", selector: "app-i18n-interp", class_name: "I18nInterpComponent" },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_ids_are_unique_and_nonempty() {
        let mut ids: Vec<&str> = CORPUS.iter().map(|f| f.id).collect();
        let n = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), n, "corpus fixture ids must be unique");
        assert!(CORPUS.iter().all(|f| !f.id.is_empty()), "every fixture needs an id");
        assert!(CORPUS.iter().all(|f| !f.class_name.is_empty()), "every fixture needs a class name");
    }

    #[test]
    fn corpus_matches_parity_mjs_count() {
        // parity.mjs's FIXTURES currently carries 5 + 10 + 8 + 4 + 2 = 29 fixtures. If this trips,
        // re-sync this corpus with libs/treaty-ivy/facade/parity/parity.mjs (the source of truth).
        assert_eq!(CORPUS.len(), 29, "corpus drifted from parity.mjs FIXTURES count");
    }
}
