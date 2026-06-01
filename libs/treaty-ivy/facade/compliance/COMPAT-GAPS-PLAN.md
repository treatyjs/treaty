# treaty_ivy — Angular compatibility gaps + plan (Phase 1 inventory)

READ-ONLY inventory of what a REAL existing Angular app needs that `treaty_ivy` does
not yet compile correctly. Two goals: (1) COMPATIBILITY (compile + run any existing
Angular app, NgModule or standalone, legacy templates, DI); (2) MODERNIZER (flag-gated
compile-time lowering of legacy → new Ivy instructions, default-on, opt-out per
sub-transform).

Reference: Angular at `tools/angular-ref`. Compliance harness at
`libs/treaty-ivy/facade/compliance` (matchGolden 170/185, 15 DIFF).

---

## A. What WORKS today for real apps

Source front-end is `libs/treaty-ivy/facade/src/source_compile.rs`
(`compile_component_source` → `compile_program_with_source`). It already:

- Walks EVERY top-level decorated class in source order (multi-class files), re-assembles
  the original ES module with decorators stripped and Ivy statics appended.
- Compiles `@Component`, `@Directive`, `@Pipe`, `@NgModule`, `@Injectable`
  (registry plugins in `decorator_registry()`); multi-decorator `@Pipe`+`@Injectable`.
- `@Component`/`@Directive` metadata: selector, inline `template`, `standalone`,
  `changeDetection`, inline `styles`, `encapsulation`, `animations`, `exportAs`,
  `host: {...}`, `@HostBinding`/`@HostListener`, `hostDirectives`, `foreignImports`.
- Inputs/outputs: `@Input`/`@Output` (alias + `transform`), signal `input()/model()/
  output()`, decorator + signal queries (`@ViewChild` &c. and `viewChild()` &c.).
- Component/directive `providers` + `viewProviders` → `ɵɵProvidersFeature` feature.
- Constructor DI: param types, `@Inject/@Optional/@Self/@SkipSelf/@Host/@Attribute`.
- `@Injectable` `providedIn/useClass/useFactory/useValue/useExisting/deps` → `ɵprov`.
- `@NgModule` `declarations/imports/exports/bootstrap/id` → `ɵmod` + guarded
  `ɵɵsetNgModuleScope` side effect (AOT full/local shape — these PASS).
- Legacy template MICROSYNTAX `*ngIf/*ngFor/*ngSwitch` etc. desugars to a `Template`
  node → `ɵɵtemplate` (compliance "should support structural directives" PASSES);
  `[ngClass]/[ngStyle]` are ordinary property bindings; `ng-template`, `ng-container`,
  `ng-content` (incl projection/fallback/ngProjectAs), `#ref` template vars,
  `let-` context vars all handled (template_transform.rs / view/template.rs).
- New control flow `@if/@for/@switch`, `@let`, `@defer` (partial), i18n blocks.
- Lifecycle detection (`ngOnChanges` → `NgOnChangesFeature`).
- Partial-declaration LINKER (`facade/src/linker.rs`) de-partials real published Angular
  packages to zero residual `ɵɵngDeclare`.

So: real STANDALONE apps with legacy or modern templates compile. The compatibility
holes below are mostly NgModule-injector, external files, and JIT-mode shape.

---

## B. Gaps, ordered by real-app impact

### G1 (CRITICAL) — `@NgModule({providers, imports})` injector (`ɵinj`) never emitted
`compile_ng_module_class` (source_compile.rs:2697) builds only `ɵmod` via
`compile_ng_module`. It does NOT read `providers` and never emits the
`ɵinj = ɵɵdefineInjector({providers, imports})` static. `compile_injector` already exists
(pipe_module_injector.rs:559) and is unused from the source path. Real impact: ANY
NgModule app that declares `providers` (root `AppModule`, feature modules, `forRoot()`)
loses its DI registrations — services won't be provided. This is the single biggest
"existing app won't run" gap. Hard: LOW–MED (read `providers`/`imports` via `convert_expr`,
call `compile_injector`, append the `X.ɵinj = …` static alongside `X.ɵmod`).

### G2 (HIGH) — external `templateUrl` / `styleUrl` / `styleUrls` rejected
source_compile.rs:2409 returns an error for `templateUrl`; there is no `styleUrl`/
`styleUrls` reading and no file resolver anywhere (`grep` finds zero external-template
resolution). Real impact: the MAJORITY of existing component files use `templateUrl`/
`styleUrls` rather than inline — they all fail to compile today. Needs an
external-resolution layer ABOVE the Rust front-end (the NAPI/bundler plugin reads the
referenced files and inlines `template`/`styles` before calling
`compile_component_source`), plus dropping the hard rejection. Hard: MED (resolution is a
host/bundler concern; the Rust change is small — accept pre-inlined template/styles and
remove the reject; optionally accept resolved strings as new metadata fields).

### G3 (MED) — `@NgModule` `schemas` / `CUSTOM_ELEMENTS_SCHEMA` dropped
`compile_ng_module_class` hard-codes `schemas: None`. Real impact: modules using
`CUSTOM_ELEMENTS_SCHEMA`/`NO_ERRORS_SCHEMA` lose the schema (template type-check leniency);
low runtime impact but a fidelity gap. Hard: LOW (read `schemas` array of identifiers like
the other scope arrays via `identifier_refs`/`refs_of`).

### G4 (LOW, compliance) — NgModule "(jit mode)" inline scope — 2 of the 6 misc-shape DIFFs
Goldens `declarations_jit_mode.js` / `imports_exports_jit_mode.js` inline
`declarations`/`bootstrap`/`imports`/`exports` DIRECTLY into `ɵɵdefineNgModule({...})`
(no `ɵɵsetNgModuleScope`). `compile_ng_module` (pipe_module_injector.rs:340) deliberately
collapses `Inline` into `SideEffect`, so it always emits the AOT side-effect form. JIT mode
is a COMPILER OPTION, not `@NgModule` metadata, so the source front-end has no signal to
switch. Real impact: NONE for real AOT apps (jit-mode is dev/test-only). To close the two
DIFFs: thread a `jit` flag and use the real inline path (`R3SelectorScopeMode::Inline`
honoured + scope arrays set on the def map). Hard: MED, LOW priority.

### G5 (LOW, compliance) — arrow function in host listener/binding — 2 misc-shape + the
`ɵɵattribute` DIFF (3 cases)
`r3_view_compiler_arrow_functions/...inside a host listener` / `...host binding` DIFF.
Host-binding/listener handler bodies containing arrow functions aren't lowered the way
ngtsc emits them. Site: host-bindings generation in `decorators/src/compiler.rs`
(`DefaultHostBindingsBuilder` path) + the host-expression converter. Hard: MED.

### G6 (LOW, compliance) — remaining template-instruction DIFFs
- `ɵɵpureFunction1` ×2: spread elements in array literals / object literals with spread
  (`value_composition`). The pure-function lowering for spread (`...x`) literals.
  Site: `core/src/expression_converter.rs` + pure-function emit in `view/template.rs`.
- `ɵɵadvance` ×2: `@let` inside i18n + child view; `@switch` inside i18n block. Advance
  index miscount in i18n + nested-view interaction. Site: `view/template.rs` advance
  bookkeeping / i18n let path.
- `ɵɵdefer` / `ɵɵelementStart` (defer): deferred block with local dependencies; `@defer`
  inside i18n. Site: `template/deferred.rs` + `view/template.rs` defer emit.
- `ɵɵdomProperty`: host binding with temporary expressions + legacyOptionalChaining. Site:
  host-binding temp-var emit in `decorators/src/compiler.rs`.
All LOW real-app priority (edge template/i18n/defer shapes); each is a localized emit fix.

---

## C. Modernizer (goal 2) — current state

There is NO modernizer flag plumbing today. The legacy `*ngIf/*ngFor/*ngSwitch` already
compile to LEGACY `ɵɵtemplate` (correct for opt-OUT). The new `@if/@for/@switch` compile to
the new instructions. To deliver the FLAG-GATED, default-on, opt-out lowering:
- Add a compile-options struct threaded source_compile.rs → template_transform.rs →
  view/template.rs (and decorators/registry `CompileCtx`).
- `*ngIf/*ngFor/*ngSwitch` → `@if/@for/@switch` r3_ast nodes BEFORE instruction emit
  (a tree transform in template_transform.rs / a new lowering pass; reuse control_flow.rs
  node shapes). Each sub-transform individually toggleable.
- `@Input/@Output/@ViewChild/@ContentChild` → `input()/output()/viewChild()/
  contentChild()` signal forms in `collect_io`/`collect_decorator_queries`
  (source_compile.rs) — emit the signal `is_signal: true` metadata instead of legacy.
This is NEW work (not a gap in existing behaviour); scope it after the compatibility gaps
G1–G3, which are required for real apps to run at all.

---

## D. Gap → site map

| Gap | File : fn | Difficulty |
| --- | --- | --- |
| G1 NgModule injector/providers | `facade/src/source_compile.rs::compile_ng_module_class` (call `decorators/src/pipe_module_injector.rs::compile_injector`) | LOW–MED |
| G2 templateUrl/styleUrl(s) | `facade/src/source_compile.rs::compile_component_or_directive` (drop reject ~line 2409); external resolution at NAPI/bundler layer | MED |
| G3 NgModule schemas | `facade/src/source_compile.rs::compile_ng_module_class` (schemas via `identifier_refs`) | LOW |
| G4 NgModule jit-inline scope | `decorators/src/pipe_module_injector.rs::compile_ng_module` + thread `jit` flag into `compile_ng_module_class` | MED (low pri) |
| G5 arrow fn in host listener/binding | `decorators/src/compiler.rs` host-bindings builder + host-expr converter | MED |
| G6 pureFunction1 spread | `core/src/expression_converter.rs` + `template/src/view/template.rs` | MED |
| G6 advance (i18n let/switch) | `template/src/view/template.rs` (advance/i18n) | MED |
| G6 defer (local deps / i18n) | `template/src/template/deferred.rs` + `template/src/view/template.rs` | MED |
| G6 domProperty temp expr | `decorators/src/compiler.rs` host-binding temp-var emit | MED |
| Modernizer (new work) | options struct threaded through `source_compile.rs` → `template_transform.rs` → `view/template.rs`; `collect_io`/`collect_decorator_queries` | LARGE |

---

## E. Recommended order

1. G1 NgModule injector/providers (real apps won't run without it).
2. G2 templateUrl/styleUrls (most real component files use external files).
3. G3 NgModule schemas.
4. Modernizer options plumbing + `*ngIf/*ngFor/*ngSwitch` → `@if/@for/@switch` lowering,
   then signal-input/query lowering.
5. Compliance DIFFs G4–G6 (raise matchGolden 170→185) as fidelity polish.
