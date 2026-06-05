# treaty_ivy — Angular Compatibility + Modernizer plan (Phase 3)

Ordered, mechanical, **green-at-every-step** execution plans for the two locked
directions (memory: `angular-compat-and-modernizer`):

1. **COMPATIBILITY** — Treaty compiles + runs *any existing* Angular app with no
   source rewrites (NgModule or standalone, legacy templates, external files, DI).
2. **MODERNIZER** — a flag-gated, **default-on / opt-out**, *in-compiler* lowering
   of legacy Angular → the new Ivy instructions. NOT a source codemod (it never
   rewrites the user's files); opting out simply emits the legacy instructions.

This plan supersedes nothing in
`libs/treaty-ivy/facade/compliance/COMPAT-GAPS-PLAN.md` (the Phase-1 gap inventory) —
it consumes it and turns it into a sequenced, test-gated work order.

Baseline (verified 2026-06-01): compliance **matchGolden 170/185, 15 DIFF**
(`libs/treaty-ivy/facade/compliance/COMPLIANCE-REPORT.md`); 447 Rust tests green;
the partial-declaration linker de-partials real `@angular/*` + CDK/Material to zero
residual `ɵɵngDeclare`.

---

## 0. Cross-cutting rules (apply to EVERY step below)

- **Serialization constraint (hard).** Both plans edit `treaty_ivy` (the 4 crates
  under `libs/treaty-ivy/`). `treaty_ivy` builds into the same `cargo target/` as the
  linker / `authoring_node` addon-rebuild workflows. **Never run these steps
  concurrently with a linker or addon-rebuild workflow** — `target/` contention is
  what hung a prior workflow (memory: `angular-compat-and-modernizer`). Take the
  build lock: one `treaty_ivy`-editing workflow at a time.
- **Parity invariant.** After every step run the compliance harness
  (`node libs/treaty-ivy/facade/compliance/run-compliance.mjs --cargo-dump`).
  `matchGolden` must **rise toward 185/185 and NEVER regress**. The 447-test Rust
  suite (`cargo test -p treaty_ivy_core -p treaty_ivy_template -p treaty_ivy_decorators -p treaty_ivy`)
  must stay green. A step is "done" only when both gates pass.
- **Commit on green each round** (memory: `commit-cadence`); **no Co-Authored-By /
  AI attribution** (memory: `no-coauthor-trailer`).
- **Architecture.** Owned IR (`Box`/`Vec`/`String`); OXC only at the emit boundary
  (memory: `render3-port-state`). Add a decorator kind / transform = a *registration*
  in the `DecoratorRegistry`, not a fork.
- **Distinct from `ngx-maintenance`.** That is a no-AI GitHub bot that *rewrites
  source files*. This modernizer is an in-compiler IR lowering with **no file
  rewrite** — the user's `.ts` is untouched on disk.

---

# PLAN A — COMPATIBILITY (8 steps)

Goal: point Treaty at an *existing* Angular workspace (NgModule or standalone, legacy
templates, `templateUrl`/`styleUrls`, providers/DI) and have it compile + link + boot.
Steps A1–A3 are the "won't run at all" gaps; A4 is the real-app boot gate; A5–A8 raise
`matchGolden` 170 → 185 as fidelity polish. Each step lists **file:fn**, the
**test/fixture** to add, and the **parity invariant**.

### A1 (CRITICAL) — NgModule injector / `providers` (`ɵinj`) [Gap G1]
**Why:** the single biggest "existing app won't run" hole. Today
`compile_ng_module_class` emits only `ɵmod`; it never reads `providers`/`imports` and
never emits `X.ɵinj = ɵɵdefineInjector({...})`. Any NgModule app declaring `providers`
(root `AppModule`, feature modules, `forRoot()`) loses every DI registration.

- **File:fn:** `libs/treaty-ivy/facade/src/source_compile.rs::compile_ng_module_class`
  (line 2697). Read `providers` (via `convert_expr` over the array) and `imports`,
  build `R3InjectorMetadata`, call the existing-but-unused
  `libs/treaty-ivy/decorators/src/pipe_module_injector.rs::compile_injector` (line 560),
  and append a second `ClassEmit` static `\u{0275}inj` next to the `\u{0275}mod` one.
- **Mechanics:** `compile_ng_module_class` currently returns ONE `ClassEmit`. Either
  (a) extend `ClassEmit` callers to accept a `Vec<ClassEmit>` for a class, or
  (b) fold the `ɵinj` assignment into `extra_statements` (it is a sibling static, not a
  side effect, so order it before the `ɵɵsetNgModuleScope` side effects). Prefer (a)
  for cleanliness; (b) if (a) ripples too far. `imports` for the injector reuse the
  same `identifier_refs` resolution already used for the module scope.
- **Test/fixture:** new compliance-style unit test in `source_compile.rs` asserting an
  `@NgModule({providers:[SvcA,{provide:TOK,useValue:1}], imports:[HttpClientModule]})`
  emits `ɵinj = ɵɵdefineInjector({providers:[...], imports:[...]})`. The golden
  `r3_compiler_compliance/ng_modules/should define an NgModule and injector with
  providers` already PASSES via the template-only path — keep it green and add the
  source-front-end assertion.
- **Parity invariant:** matchGolden ≥ 170 (no regression); the injector golden stays
  PASS; new unit test green.
- **Difficulty:** LOW–MED.

### A2 (HIGH) — external `templateUrl` / `styleUrl` / `styleUrls` [Gap G2]
**Why:** the *majority* of real component files use external HTML/CSS, not inline.
Today `compile_component_or_directive` (source_compile.rs:2393) hard-rejects at line
2409 (`"external templateUrl unsupported"`), and there is no `styleUrl(s)` reading.

- **Design (load-bearing):** external file *resolution* is a HOST/bundler concern, not
  a Rust-compiler concern (the Rust front-end is a pure source→Ivy function). So:
  - **Bundler/NAPI layer (above Rust):** before calling `compile_component_source`,
    the addon resolves `templateUrl`/`styleUrls` relative to the component file, reads
    the referenced files, and **inlines** them as `template`/`styles` metadata. This is
    new TS-shim glue in the bundler plugin (`apps/treaty-cli/src/plugin.rs` /
    `bundler.rs`) + the NAPI surface.
  - **Rust change (small):** in `compile_component_or_directive`, remove the hard
    reject and instead, if `template` is absent but a pre-resolved template string was
    supplied, use it; same for `styles`/`styleUrls`. Accept resolved strings as
    new optional metadata fields rather than reading files in Rust.
- **File:fn:** `source_compile.rs::compile_component_or_directive` (drop reject at
  ~2409, add resolved-string acceptance); resolution glue in
  `apps/treaty-cli/src/plugin.rs` + `libs/authoring/node/src/lib.rs`
  (`compile_component_source` gains a resolved-template/styles channel — see A2-NAPI).
- **A2-NAPI:** `compile_component_source(source: String)` currently takes no options
  (`libs/authoring/node/src/lib.rs:59`). Add an additive options object
  (`{ resolvedTemplate?: string, resolvedStyles?: string[] }`) — the SAME object the
  Modernizer flags (Plan B) will extend, so design it once (see B0).
- **Test/fixture:** a fixture component pair (`c.ts` with `templateUrl:'./c.html'`,
  `styleUrls:['./c.css']` + the two files); assert it compiles with the template/styles
  inlined and matches the inline-equivalent golden. Add a Rust unit test that the
  resolved-string path produces identical output to the inline form.
- **Parity invariant:** matchGolden ≥ 170; the existing templateUrl rejection test
  (`source_compile.rs:3504`) is updated to assert resolution succeeds when a resolved
  template is supplied (and still errors when neither inline nor resolved is present).
- **Difficulty:** MED (Rust small; resolution is host glue).

### A3 (MED) — `@NgModule` `schemas` / `CUSTOM_ELEMENTS_SCHEMA` [Gap G3]
**Why:** modules using `CUSTOM_ELEMENTS_SCHEMA`/`NO_ERRORS_SCHEMA` lose the schema
(template type-check leniency). Today `compile_ng_module_class` hard-codes
`schemas: None` (source_compile.rs:2730).

- **File:fn:** `source_compile.rs::compile_ng_module_class` — read `schemas` as an array
  of bare identifiers via the existing `identifier_refs`/`refs_of` helper (already used
  for `declarations`/`imports`), map to the `schemas` field of `R3NgModuleCommon`.
- **Test/fixture:** unit test that `@NgModule({schemas:[CUSTOM_ELEMENTS_SCHEMA]})`
  threads the schema into the def.
- **Parity invariant:** matchGolden ≥ 170 (no regression).
- **Difficulty:** LOW.

### A4 (GATE) — vendor real sample apps + "compiles + links + boots" gate
**Why:** the memory bar is *measured against REAL apps*, not just the golden corpus.

- **Vendor two minimal real Angular apps as compat fixtures** under
  `libs/treaty-ivy/facade/compliance/fixtures/apps/`:
  - `ngmodule-app/` — an `AppModule`-based app (`@NgModule` bootstrap,
    `declarations`/`providers`, a component using `*ngIf`/`*ngFor`, a service injected
    via constructor DI, an external `templateUrl`).
  - `standalone-app/` — a `bootstrapApplication` standalone app (standalone components,
    `@if`/`@for`, signal `input()`/`output()`, `provideX` providers).
  Keep them MINIMAL (a handful of files each) and pin the Angular version (v22).
- **Gate harness (new mjs, sibling of `run-compliance.mjs`):** for each vendored app
  (1) compile every component/module/service via the source front-end → assert zero
  errors; (2) link its `@angular/*` deps via the partial-declaration linker
  (`facade/src/linker.rs`) → assert **zero residual `ɵɵngDeclare`**; (3) a "boots"
  smoke: bundle the compiled output (reuse `examples/linker-smoke/e2e.mjs` machinery)
  and assert the app's bootstrap entry evaluates without throwing.
- **Sequencing:** runs AFTER A1–A3 (the ngmodule-app needs `ɵinj` + `templateUrl`).
- **Parity invariant:** the two-app gate goes from RED → GREEN and stays green; it is
  the canonical "100% parity vs real apps" signal going forward.
- **Difficulty:** MED (mostly harness/fixture authoring).

### A5 (LOW, compliance) — NgModule "(jit mode)" inline scope [Gap G4, 2 DIFFs]
Goldens `declarations_jit_mode.js` / `imports_exports_jit_mode.js` inline scope arrays
directly into `ɵɵdefineNgModule({...})` (no `ɵɵsetNgModuleScope`). `compile_ng_module`
(pipe_module_injector.rs:340–357) deliberately collapses `Inline` → `SideEffect`.

- **File:fn:** thread a `jit` compiler flag (a *compile option*, not `@NgModule`
  metadata) from the options struct (B0) into `compile_ng_module_class`, and honour
  `R3SelectorScopeMode::Inline` in `compile_ng_module`.
- **Test/fixture:** the two jit-mode goldens flip DIFF → PASS.
- **Parity invariant:** matchGolden 170 → 172.
- **Difficulty:** MED; LOW priority (jit mode is dev/test-only; no real-AOT-app impact).

### A6 (LOW, compliance) — arrow fn in host listener/binding [Gap G5, 3 DIFFs]
`misc-shape` ×2 + `ɵɵattribute` ×1: host-binding/listener bodies containing arrow
functions aren't lowered the way ngtsc emits them.

- **File:fn:** `libs/treaty-ivy/decorators/src/compiler.rs` (the
  `DefaultHostBindingsBuilder` path) + the host-expression converter.
- **Test/fixture:** the two `r3_view_compiler_arrow_functions/...host listener|host
  binding` cases flip to PASS.
- **Parity invariant:** matchGolden +3.
- **Difficulty:** MED.

### A7 (LOW, compliance) — remaining template-instruction DIFFs [Gap G6]
Localized emit fixes, each its own sub-step (verify one, commit, next):
- `ɵɵpureFunction1` ×2 — spread elements in array / object literals (`value_composition`).
  **File:** `core/src/expression_converter.rs` + pure-fn emit in
  `template/src/view/template.rs`.
- `ɵɵadvance` ×2 — `@let` inside i18n + child view; `@switch` inside i18n.
  **File:** `template/src/view/template.rs` (advance bookkeeping / i18n let path).
- `ɵɵdefer` / `ɵɵelementStart` ×2 — deferred block with local deps; `@defer` inside i18n.
  **File:** `template/src/template/deferred.rs` + `template/src/view/template.rs`.
- `ɵɵdomProperty` ×1 — host binding with temporary expressions + legacyOptionalChaining.
  **File:** `decorators/src/compiler.rs` host-binding temp-var emit.
- **Parity invariant:** matchGolden +7 toward 185.
- **Difficulty:** MED each.

### A8 (LOW, compliance) — `ɵɵsyntheticHostListener` (animation host binding) [1 DIFF]
`r3_view_compiler_styling/component_animations/should generate animation host binding
and listener code for directives`.
- **File:fn:** `decorators/src/compiler.rs` host-listener emit (synthetic `@`-prefixed
  animation listener).
- **Parity invariant:** matchGolden → **185/185** when combined with A5–A7.
- **Difficulty:** MED.

**Plan A score arithmetic:** A5(+2) + A6(+3) + A7(+7) + A8(+1) = +13 ⇒ **170 → 183**.
The remaining 2 of the 15 are the jit-mode pair (A5) — so A5+A6+A7+A8 close all 15 and
reach **185/185**. (The report's 15 DIFFs decompose as: 6 misc-shape = 2 jit + 2 arrow +
1 domProperty + 1 elementStart-defer; 2 advance; 2 pureFunction1; 1 syntheticHostListener;
1 defer; 1 attribute.)

---

# PLAN B — MODERNIZER (6 steps)

Goal: a flag-gated, default-on, opt-out, per-sub-transform-toggleable **in-compiler
lowering** of legacy → new Ivy. Implemented as **registered IR-transform passes** over
the template/metadata IR *before* emit — NOT a compiler fork, NOT a source rewrite.

## DEFAULT BEHAVIOUR (LOCKED with user 2026-06-01): CONSERVATIVE, SAFE-ONLY, AUTO-DETECT

The modernizer NEVER rewrites user method bodies. Defaults:
- **Always-safe, default-ON:** template control-flow (`*ngIf`/`*ngFor`/`*ngSwitch` →
  `@if`/`@for`/`@switch`); `@Output()` → `output()` (side-effect-free, read unchanged);
  default→OnPush when the component declares no explicit `changeDetection`.
- **Conditionally default-ON (auto-detect SAFETY GATE):** `@Input()` → `input()` and
  `@ViewChild`/`@ContentChild` → `viewChild()`/`contentChild()` are signalized ONLY for
  members with NO imperative `this.<member>` read in the class body (a read would change
  from `this.x` to `this.x()`). If any imperative read of that member exists, the compiler
  SKIPS signalizing it and emits the correct legacy `@Input`/query form. Requires a
  read-detection pass over the class body (ctor/methods/lifecycle) per candidate member;
  member is signal-eligible iff never read imperatively (template reads are fine — the
  template lowers in lockstep). NEVER rewrite call-sites (that is source-rewriting =
  [[ngx-maintenance]]'s job, not this in-compiler lowering).
- Every transform individually toggleable; all opt-OUT. A member skipped by the safety
  gate is REPORTED (diagnostic), not silently downgraded.

**The decisive architectural fact** (verified): legacy and modern forms already converge
on the SAME downstream emit, so the modernizer is a pure IR remap onto the
already-correct, already-tested control-flow node types:
- Legacy `*ngIf`/`*ngFor`/`*ngSwitch` desugar in
  `template/src/template/template_transform.rs::wrap_in_template` (line 1253; sites 488,
  629) into a `t::Template` node, which `template/src/view/template.rs` lowers to the OLD
  `ɵɵtemplate` instruction.
- Native `@if`/`@for`/`@switch` parse (via `control_flow.rs::create_if_block` :301 /
  `create_for_loop` :382 / `create_switch_block` :482) into
  `t::IfBlock`/`t::ForLoopBlock`/`t::SwitchBlock`, which `view/template.rs` lowers to the
  NEW `ɵɵconditionalCreate`/`ɵɵconditional` / `ɵɵrepeaterCreate`/`ɵɵrepeater`
  instructions.
- So the modernizer's template pass is a `Vec<t::Node>` → `Vec<t::Node>` rewrite that
  maps legacy `t::Template` shapes onto the SAME `IfBlock`/`ForLoopBlock`/`SwitchBlock`
  structs `control_flow.rs` already produces (all fields confirmed present on
  `r3_ast.rs` lines 341/397/404/419/436/458/474). **`control_flow.rs` is untouched** — it
  stays the canonical producer; the modernizer just feeds its output types. Emit is
  unchanged and already proven by the passing `@if`/`@for`/`@switch` goldens.

### B0 (FOUNDATION) — options struct + flag plumbing (NAPI + bundler)
**Why:** every later step is gated by this; build it once and share it with A2/A5.

- **Rust options struct** (new, in `facade/src` or a small `treaty_ivy` options module),
  threaded `source_compile.rs` → `template_transform.rs` → `view/template.rs` and into
  the decorators `CompileCtx`:
  ```rust
  pub struct ModernizeOptions {
      pub control_flow: bool,   // *ngIf/*ngFor/*ngSwitch -> @if/@for/@switch
      pub signal_inputs: bool,  // @Input/@Output -> input()/output()
      pub signal_queries: bool, // @ViewChild/@ContentChild -> viewChild()/contentChild()
      pub on_push: bool,        // default CD -> OnPush where safe
  }
  // Default impl: ALL true (default-on, opt-out).
  ```
  Plus the `jit` flag (A5) and `resolvedTemplate`/`resolvedStyles` (A2) live on the same
  parent `CompileOptions` so there is ONE threaded options object.
- **NAPI surface** (`libs/authoring/node/src/lib.rs`): `compile_component_source` gains
  an optional `#[napi(object)]` `CompileOptions` arg (additive; the existing zero-arg
  callers keep working via `Option`/default). Regenerate `index.d.ts`.
- **Bundler options** (`apps/treaty-cli/src/plugin.rs` / `bundler.rs` /
  `config.rs`): expose `modernize: { controlFlow, signalInputs, signalQueries, onPush }`
  (default-on) in the Treaty config and pass it through to the NAPI call.
- **Test/fixture:** a round-trip test that flags default to all-on, and that explicitly
  disabling each flag is honoured end-to-end (Rust unit + a NAPI smoke).
- **Parity invariant:** with all flags at their compatibility-equivalent setting the
  output is byte-identical to today (matchGolden unchanged). **Critical:** the compliance
  harness must run the modernizer in its *opt-OUT* configuration so the 170/185 baseline
  is the legacy-emit baseline and cannot silently shift.
- **Difficulty:** MED (plumbing breadth, not depth).

### B1 — template control-flow lowering (`*ngIf`/`*ngFor`/`*ngSwitch` → `@if`/`@for`/`@switch`)
Runs AFTER `html_ast_to_render3_ast` (source_compile.rs ~2937) and BEFORE the nodes flow
into `compile_component_meta` / the view builder. Gated by `ModernizeOptions.control_flow`.

- **File:fn:** new lowering pass (e.g. `template/src/template/modernize.rs`, registered
  and invoked from `source_compile.rs`); pattern-matches each `t::Template` whose
  `template_attrs`/inputs carry an `ngIf`/`ngFor`/`ngSwitch`-shaped binding and
  reconstructs the equivalent `IfBlock`/`ForLoopBlock`/`SwitchBlock`, then recurses into
  children. **Does not touch `control_flow.rs`.**
- **Mapping detail (what the rewrite synthesizes):**
  - `*ngIf="cond"` (+ `ngIfElse`/`ngIfThen`/`ngIfThenElse` template-ref siblings) →
    `IfBlock { branches: [IfBlockBranch{ expression: Some(cond), children: <tpl body>,
    expression_alias: <from `as` if present> }, <else branch from elseRef>] }`.
  - `*ngFor="let item of items; let i = index; trackBy: fn; ..."` →
    `ForLoopBlock { item, expression: items, track_by: Some(fn) (else identity),
    context_variables: [$index/$count/$first/$last/$even/$odd from the let-aliases],
    children: <tpl body>, empty: None }`. (Note: legacy `*ngFor` has no `@empty`; the
    new emit's required `track` defaults to `$index`-identity when no `trackBy`.)
  - `[ngSwitch]="expr"` host + `*ngSwitchCase="v"` / `*ngSwitchDefault` children →
    `SwitchBlock { expression: expr, groups: [SwitchBlockCaseGroup{ cases:[..], children
    }], ... }`.
- **Before/after Ivy emit (opt-out vs default-on), `*ngIf="show"`:**
  - opt-out (legacy): `ɵɵtemplate(0, MyCmp_ng_template_0_Template, ...); ... ɵɵproperty("ngIf", ctx.show)`
  - default-on (modern): `ɵɵconditionalCreate(0, MyCmp_Conditional_0_Template, ...); ... ɵɵconditional(ctx.show ? 0 : -1)`
- **Test/fixture:** for each of `*ngIf`/`*ngFor`/`*ngSwitch`, a unit test asserting the
  modernized emit equals the corresponding *hand-written* `@if`/`@for`/`@switch` golden
  (reuse the passing native-control-flow goldens as oracles), AND that with
  `control_flow:false` the legacy `ɵɵtemplate` emit is byte-identical to today.
- **Parity invariant:** harness (opt-out) stays 170; a new "modernized == native"
  oracle test set goes green.
- **Difficulty:** MED–LARGE (the microsyntax → block-shape mapping; emit is free).

### B2 — signal inputs/outputs (`@Input`/`@Output` → `input()`/`output()`)
Gated by `ModernizeOptions.signal_inputs`. The metadata path already emits the
`is_signal: true` shape for native signal `input()`/`output()`/`model()` (compliance
`signal_inputs/*` PASS), so this remaps the legacy decorator metadata onto that path.

- **File:fn:** `source_compile.rs::collect_io` (line 618) — when `signal_inputs` is on,
  emit each `@Input`/`@Output` with the signal flag set (`is_signal: true`) and the
  signal-input/output instruction shape instead of the legacy decorator form.
- **Before/after (component with `@Input() name: string`):**
  - opt-out: `inputs: { name: "name" }` (zone/decorator input)
  - default-on: `inputs: { name: [InputFlags.SignalBased, "name"] }` (mirrors the
    `signal_inputs` golden shape)
- **Caveat (correctness gate):** signalizing a `@Input` changes the *runtime read*
  (call-site `this.name` → `this.name()`). Pure compile-time lowering of the
  *definition* is safe; but the **template + class body still read it as a plain field**.
  Therefore B2 must be limited to the DEFINITION shape only when the field is not
  read imperatively in a way that would break, OR scoped to inputs already consumed
  signal-style. **Document this as the key risk** (see Notes) — the safe default may be
  to keep B2 OFF-by-default-in-practice for `@Input` unless paired with call-site
  rewriting (which would cross into source-rewrite territory we are avoiding). Ship the
  toggle; default-on only for output() (side-effect-free) and gate input() behind a
  "definition-only / no imperative read" safety check.
- **Test/fixture:** modernized `@Input/@Output` emit == `model_inputs`/`output_function`
  golden shapes; opt-out == legacy.
- **Parity invariant:** harness (opt-out) stays 170.
- **Difficulty:** MED (definition remap easy; the safety analysis is the work).

### B3 — queries (`@ViewChild`/`@ContentChild` → `viewChild()`/`contentChild()`)
Gated by `ModernizeOptions.signal_queries`. The signal-query emit already exists
(compliance `signal_queries/*` PASS).

- **File:fn:** `source_compile.rs::collect_decorator_queries` (line 957) — when
  `signal_queries` is on, emit the signal-query metadata (`ɵɵviewQuerySignal` /
  `ɵɵcontentQuerySignal` + `queryAdvance`) instead of the legacy
  `ɵɵviewQuery`/`ɵɵcontentQuery` form.
- **Same read-site caveat as B2** (a `@ViewChild` field becomes a signal getter).
  Same mitigation: definition-only remap + safety gate; default-on only where safe.
- **Before/after:** opt-out `ɵɵviewQuery(_c0, 5); ... ɵɵqueryRefresh(...) && (ctx.x = ...)`
  → default-on `ɵɵviewQuerySignal(ctx.x, _c0, 5); ... ɵɵqueryAdvance()`.
- **Test/fixture:** modernized query emit == `signal_queries` golden; opt-out == legacy.
- **Parity invariant:** harness (opt-out) stays 170.
- **Difficulty:** MED.

### B4 — default `changeDetection` → OnPush where safe
Gated by `ModernizeOptions.on_push` (aligns with memory `treaty-signals-by-default`).

- **File:fn:** the component-metadata assembly in
  `source_compile.rs::compile_component_or_directive` — when `on_push` is on and the
  component does NOT explicitly set `changeDetection`, default to
  `ChangeDetectionStrategy.OnPush` in the emitted def.
- **"Where safe":** only flip when no explicit strategy is given. Do NOT override an
  explicit `Default`. This is the highest-risk-of-behaviour-change transform, so it is
  individually toggleable and the most conservative default candidate.
- **Before/after:** opt-out — no `changeDetection` key (Default) → default-on —
  `changeDetection: 0 /* OnPush */` in `ɵɵdefineComponent`.
- **Test/fixture:** a component with no explicit strategy emits OnPush under the flag,
  Default with it off; an explicit-`Default` component is never overridden.
- **Parity invariant:** harness (opt-out) stays 170.
- **Difficulty:** LOW (emit) / the *semantics* are the risk.

### B5 — wire modernizer into the boot gate + docs
- Run the A4 vendored apps through the gate with the modernizer **default-on** and assert
  they still compile + link + boot (modern emit), then again **fully opt-out** and assert
  legacy emit also boots. This proves both directions of the toggle on real apps.
- Document the flags + the read-site caveat in the Treaty config docs.
- **Difficulty:** LOW (harness/docs).

---

## C. Dependency order (combined)

```
A1 (ɵinj) ─┐
A2 (tplUrl)├─► A4 (real-app boot gate) ─────────────► A5/A6/A7/A8 (DIFFs → 185)
A3 (schema)┘
B0 (options/flags) ─► B1 (control-flow) ─► B2 (inputs) ─► B3 (queries) ─► B4 (OnPush) ─► B5 (gate+docs)
```
A1–A4 are the real-app blockers and come first. B0 can start in parallel ONLY if a
separate `treaty_ivy` build window is available (serialization rule) — otherwise
interleave A then B. A5–A8 (fidelity DIFFs) are independent and can slot anywhere after
A4.

## D. Step / score summary
- **Plan A: 8 steps.** A1–A3 close the "won't run" gaps (G1–G3); A4 is the real-app
  compiles+links+boots gate (vendor 1 NgModule + 1 standalone app); A5–A8 raise
  compliance **170/185 → 185/185** (close all 15 DIFFs), never regressing.
- **Plan B: 6 steps.** B0 options/flag plumbing (NAPI + bundler, default-on/opt-out,
  per-transform toggle, shared with A2/A5); B1 control-flow lowering (the
  architecturally-free win — remaps onto `control_flow.rs` output types); B2 signal
  inputs/outputs; B3 signal queries; B4 OnPush default; B5 boot-gate wiring + docs.
