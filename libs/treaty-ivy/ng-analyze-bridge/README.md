# `treaty_ivy_ng_bridge` — Angular's own decorator analyser, as an opt-in source of Treaty's decorator facts

**Status: enabled in the workspace, experimental, opt-in.** The default build does not use it. With
`--features ng-analyze` (on `treaty_ivy`) the component and directive metadata comes from Angular's Rust
analyser (`ng-analyze`) where it can, and from Treaty's own extractor everywhere else. The workspace resolves
with the bridge in its graph (see [Resolved: the workspace resolves](#resolved-the-workspace-resolves-with-the-bridge)).
Every number below was measured in the real tree.

## What is reused from Angular

`ng-analyze` (`packages/compiler-cli/preprocessor/ng-analyze`, MIT, built on oxc 0.144) analyses
TypeScript sources and reports, per decorated class:

- `@Component` / `@Directive` options: `selector` (including imported constants), `standalone`, `signals`,
  `exportAs`, `host` (folded, plus the un-folded property list), `hostDirectives`, `providers`,
  `viewProviders`, `animations` (as spans), `encapsulation`, `changeDetection`, `preserveWhitespaces`,
  `template` / `templateUrl`, `styles` / `styleUrl(s)`, `imports`.
- Members: `@Input` / `@Output` / `@HostBinding` / `@HostListener` / `@ViewChild*` / `@ContentChild*`,
  signal `input()` / `model()` / `output()` / `viewChild()` / `contentChild*()`, `ngOnChanges`.
- `@Pipe` (name, pure, standalone), `@Injectable` (providedIn, use*, deps), `@NgModule` (declarations,
  imports, exports, bootstrap, providers, schemas).
- Constructor parameters with their decorators (`@Inject`, `@Optional`, `@Self`, `@SkipSelf`, `@Host`,
  `@Attribute`).
- Static evaluation of values and cross-file resolution of imports, through its own overlay file system.

## What is not usable here, and why

| Angular piece | Why it is not used |
| --- | --- |
| TS source-rewrite data (`removal_spans`, `nocollapse_insertions`, `coercion_members`, `signal_debug_names`, `imports` declaration pruning) | Treaty emits its own output and never rewrites the TypeScript source. |
| Whole-program analysis (`Analyzer` over a tsconfig and project) | Treaty compiles one source at a time. The bridge gives ng-analyze a virtual project per source, with a synthetic tsconfig. |
| Import emit strategies (`ReferenceEmitStrategy`, `PrefixImportStrategy`, the `i0` namespace, `typecheck_import`) | Treaty writes its own import lines. The reconstructed specifiers are never copied into output. |
| Disk resource loading (`ResourceResolver`) | Templates and styles are supplied as virtual files from Treaty's bundler channel, so both extractors read identical bytes. |
| NAPI and Wasm bindings | Not needed for an in-process Rust call. |
| Template-level data | ng-analyze does not parse templates. Treaty's template pipeline is unchanged. |

## Upstream status

Angular's own caution, in `packages/compiler-cli/preprocessor/README.md`, reads: the code is an
"early-stage prototype under active development by the Angular team", and external issues and pull
requests are not accepted. So every gap found here is handled in this adapter, not reported upstream.

## Conventions the bridge applies on top of ng-analyze

The facts are raw (what the author wrote). The facade applies these conventions when it turns facts into
metadata (`treaty_ivy::source_compile::facts_emit`):

- a selectorless `@Component` takes the filename-derived default selector;
- `signals` is the explicit `signals` option only, the same as Treaty's own extraction and ngtsc's `isSignal`
  (`directive/src/shared.ts`). A signal input or query does not set it;
- `changeDetection` is reduced to `Default`/`Eager` versus everything else (OnPush);
- `modernize.signal_inputs` lowers decorator inputs to signal inputs;
- `forwardRef` in `imports:` closure-wraps the dependencies (read from the decorator object).

ng-analyze's own defaults are reported as absent, so that facts match what the author wrote: the default
component `selector` (`ng-component`) and the class name substituted for an absent pipe `name`.

The adapter also corrects three ng-analyze reports, each covered by an adapter test:

- `template` holds the loaded content of a `templateUrl`; the inline `template` is reported only when
  no `templateUrl` is declared;
- `changeDetection` holds the evaluated number (`1`); it is mapped back to `Default`;
- an entry of `host` that does not fold is dropped from `host_metadata`; the complete `host_properties`
  list is read instead, so it is kept as an expression.

## Parity with Treaty's own extraction (measured)

`facade/tests/ng_analyze_parity.rs` (run with `--features ng-analyze`) extracts both sets of facts for
every `.ts` file of the Angular compliance corpus, normalises them (expressions lowered through Treaty's
converter, query selector lists split as `query_generation.ts` does, `changeDetection` reduced to its
semantic value, empty `providers` equal to absent) and diffs them field by field. A difference fails the
test unless `classify` gives it a written reason.

Measured at Angular `78d2a67d`, after the Part 2 fixes:

| | count |
| --- | --- |
| corpus `.ts` files | 644 |
| files with decorated classes | 636 |
| classes compared (matched by name and occurrence) | 1121 |
| both extractors extract the class | 1117 |
| both refuse the class (both fall back) | 4 |
| bridge refuses or omits, Treaty extracts | 1 (`@NgModule({jit: true})`: ng-analyze reports no class; the facade falls back) |
| Treaty refuses, bridge extracts | **0** (was 5 before the Part 2 fixes) |
| un-explained differences | **0** |
| field differences accepted with a written reason | **8** (was 29 before the Part 2 fixes) |

The 8 accepted differences, each with the reason `classify` records:

| Field | Count | Which extractor is right, and why |
| --- | --- | --- |
| `directive.inputs.transform` | 6 | **Treaty right.** ng-analyze reports a signal input's `transform`; Angular's definition does not capture it (golden `signal_inputs/should not capture the transform function`). |
| `pipe.name` | 1 | **Treaty right.** ng-analyze substitutes the class name for an absent pipe `name`; ngtsc errors. The bridge cannot tell the two apart. |
| `ng_module.imports` | 1 | **Neither, by design.** A `ModuleWithProviders` call is resolved to its module by ngtsc; Treaty resolves it at emit, not in its facts. |

Treaty defects found by the parity test, and their state now (each has a unit test in the facade, and its
entry was removed from `classify`, so the test enforces agreement):

| Defect | Angular behaviour | State |
| --- | --- | --- |
| Decorator-level `inputs: [...]` / `outputs: [...]` ignored | read by `parseInputsArray` / `parseOutputsArray` (`directive/src/shared.ts`) | **fixed** (`collect_decorator_io_arrays`) |
| A member with `@Input` and `@Output` loses the output | both registered (`parseInputFields`, `parseOutputFields`) | **fixed** (`collect_io`) |
| `signals: true` emitted implicitly for signal inputs/queries | `isSignal` is the explicit option only | **fixed** in both emit paths; the modernizer test follows it |
| Identifier-valued `template`, `selector`, `exportAs`, `styles`, pipe `name` refused | evaluated (`non_literal_template*.ts`) | **fixed** for same-file constants (`source_compile/const_fold.rs`); cross-file constants stay bridge-only |
| Non-literal `host` value refused | an expression is kept (`hostAttrs: ["foo", BAR]`, `host_with_ts_expression_node.ts`) | **fixed** for plain attributes; `[prop]`/`(event)` keep the string requirement, as ngtsc |
| `deps` with the qualifier first (`[new Optional(), Dep]`) refused | markers read in any position | **fixed** |
| Namespace-member `hostDirectives` (`ns.Dir`) dropped silently | resolved by ngtsc | **open** (Treaty drops it; the bridge leaves the name empty, so the facade falls back). Covered by `host_directive_names_are_local_bindings` |

Not found by the parity test, found by the golden work: a `useFactory` arrow whose body is `new X(...)` was
refused (Treaty had no `new` in `convert_expr`), and host property names were not DOM-remapped
(`[tabindex]` must emit `"tabIndex"`, Angular's `DOM_PROPERTY_REMAPPING` for DomOnly host bindings). Both are
fixed in the facade.

## Golden results with the feature on and off (same moment, same tree)

| Bucket | Feature off | Feature on |
| --- | --- | --- |
| Full (`corpus_dump`, compile-verified) | 189 PASS / 0 DIFF of 189 runnable | **189 PASS / 0 DIFF** of 189 runnable |
| Partial (template goldens, `expectEmit`) | 351 runnable, **207 PASS**, 144 DIFF | 351 runnable, **207 PASS**, 144 DIFF |

Feature on versus off, per case: 0 gained, 0 lost. Before the same-file constant folding the bridge gained
three cases (`should support inline non-literal templates`, `… using string concatenation`, `… using
substitution`); Treaty now passes them with the feature off, so the bridge has nothing left to add there.
The bridge's remaining value on the corpus is the parity check, not a golden gain.

The partial bucket's DIFF list is the same with the feature on and off (144 cases). The class-shape group
(`ClassShape` in the facade: `static ɵfac`/`static ɵcmp` inside the class body, as Angular's goldens show) is not
the default yet, because `libs/packagr/src/component_dts.rs` reads the assignment form. With
`TREATY_IVY_CORPUS_CLASS_SHAPE=static` the corpus is scored against the static shape: 213 PASS, 138 DIFF, and no
case moves the other way.

With the feature on, `ng_analyze_counters()` on the corpus dump reports **734 facts-path classes** and 14
fallbacks: 10 classes whose external `templateUrl`/`styleUrls` the dump does not resolve (ng-analyze's file
error; the corpus dump passes no resolved content) and 4 classes with a `foreignImports` key (a Treaty-only key).

Facade differential tests (`facade/tests/ng_analyze_facts_path.rs`) compile each snippet twice in one
process, once through the facts path and once with `with_treaty_extraction`: all 10 produce identical code
and errors.

## Scope of the emit path

With the feature on, the facts path emits **components and directives**. Pipes, injectables and NgModules
are still extracted by Treaty's walk, and their facts are produced and checked by the parity test. Wiring them
through the facts path was analysed and not done, for three measured reasons:

- **Pipes**: ng-analyze reports the class name for an absent `name`, so its pipe name cannot be used as
  the emit input without the decorator object. Treaty's own reading is the right one, so the facts add nothing.
- **Injectables**: the facts match Treaty's extraction field for field on the corpus (0 differences), so a
  facts path would change no output. The facts also drop the `forwardRef` wrapper flag, which the emit needs.
- **NgModules**: the facts omit a `ModuleWithProviders` call in `imports` (ngtsc resolves it); a facts path would
  need to fall back for any call element, and the registry's `CompileCtx` (in `treaty_ivy_decorators`, not
  owned by this crate) has no field to carry the facts to the plugins. That field is the exact edit needed.

## Resolved: the workspace resolves with the bridge

The blocker was `libs/runtime`'s `nova_vm` pin: `bece61ac` pins `hashbrown =0.17.0`, while `oxc_allocator`
0.144 (through ng-analyze) needs `^0.17.1`. The fix is the nova rev `4eea7c6f` (upstream `main`, `hashbrown
=0.17.1`) plus a lock update for `hashbrown`. The runtime's behaviour at the new rev is unchanged: 483 tests pass
before and after, the node-conformance suite is unchanged (51 unit tests; corpus 68 pass, 0 fail, 6 skipped),
and no runtime source changed. Enabling the bridge then needed only: the bridge as a workspace member, the
optional dependency and the `ng-analyze` feature on `treaty_ivy`.

## Tests

- `cargo test -p treaty_ivy_facts` and `cargo test -p treaty_ivy_ng_bridge`: span conversion (non-ASCII),
  nothing needs ng-analyze.
- `cargo test -p treaty_ivy_ng_bridge --features analyze`: adapter tests on small sources and on the
  assessment fixtures in `tests/fixtures/project` (18 tests).
- `cargo test -p treaty_ivy --features ng-analyze --test ng_analyze_parity -- --nocapture` (with
  `NG_ANALYZE_PARITY_REPORT=<path>` for the JSON report) and `--test ng_analyze_facts_path`.

## Pinned rev and how to bump it

The git dependency is pinned in `Cargo.toml` to `78d2a67d922b2a2af29d50aed587bc78a99a5450`, the commit
in `tools/render3-sync/ANGULAR_REF`. **Bump both together.** Then:

1. Change `rev` in `libs/treaty-ivy/ng-analyze-bridge/Cargo.toml` and `tools/render3-sync/ANGULAR_REF`.
2. Re-run the parity test and the feature-on golden. Every new explained difference needs a reason in
   `classify` (in the parity test), and any new un-explained one is a finding, not a reason to widen it.
3. Check the oxc and Rust requirements: ng-analyze declares no `rust-version`, and its oxc 0.144 crates
   and `oxc_resolver` 11.24.3 declare `rust-version = "1.95.0"`, the pinned toolchain.
