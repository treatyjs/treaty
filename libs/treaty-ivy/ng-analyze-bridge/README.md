# `treaty_ivy_ng_bridge` — Angular's own decorator analyser, as an opt-in source of Treaty's decorator facts

**Status: experimental and opt-in.** The default build does not use it. With `--features ng-analyze` the
component and directive metadata comes from Angular's Rust analyser (`ng-analyze`) where it can, and from
Treaty's own extractor everywhere else. **Enabling it in this workspace is currently blocked** (see
[Blocker](#blocker-the-workspace-cannot-resolve-ng-analyze-yet)); what is verified below was built and
tested in a scratch copy of the workspace with that one change applied.

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
- `signals` is the explicit option OR any signal input or query (Treaty's emit convention; ngtsc's
  `isSignal` is the explicit option only);
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

Measured at Angular `78d2a67d`:

| | count |
| --- | --- |
| corpus `.ts` files | 644 |
| files with decorated classes | 636 |
| classes compared (matched by name and occurrence) | 1121 |
| both extractors extract the class | 1112 |
| both refuse the class (both fall back) | 4 |
| bridge refuses or omits, Treaty extracts | 1 (`@NgModule({jit: true})`: ng-analyze reports no class; the facade falls back) |
| Treaty refuses, bridge extracts | 5 (see below) |
| un-explained differences | **0** |
| field differences accepted with a written reason | 29 (see the table below) |

Explained differences, grouped by field, with the reason each one is expected:

| Field | Count | Which extractor is right, and why |
| --- | --- | --- |
| `directive.signals_emitted` | 16 | **Treaty defect (open).** Treaty emits `signals: true` for a class that has a signal input or query but no explicit `signals` option. ngtsc's `isSignal` is the explicit option only (`directive/src/shared.ts`, lines 401-408). No full golden expects the implicit flag (the 14 corpus classes that get it are partial or signal-only goldens), so the Angular source is the evidence. Kept as Treaty's convention in the facts path until the owner decides. |
| `directive.inputs.presence` | 4 | **Treaty defect.** Decorator-level `inputs: [...]` is not read by Treaty; ngtsc reads it (`directive/src/shared.ts`, `inputsField`). |
| `directive.outputs.presence` | 1 | **Treaty defect.** A member with both `@Input` and `@Output` loses its output in `collect_io`. |
| `directive.inputs.transform` | 6 | **Treaty right.** ng-analyze reports a signal input's `transform`; Angular's definition does not capture it (golden `signal_inputs/should not capture the transform function`). |
| `pipe.name` | 1 | **Treaty right.** ng-analyze substitutes the class name for an absent pipe `name`; ngtsc errors. The bridge cannot tell the two apart, so pipes stay on Treaty's path. |
| `ng_module.imports` | 1 | **Neither, by design.** A `ModuleWithProviders` call is resolved to its module by ngtsc; Treaty resolves it at emit, not in its facts. |
| Treaty refuses an identifier-valued `template` (`template: CONST`) | 3 | **ng-analyze right.** ngtsc evaluates the constant. |
| Treaty refuses a non-literal `host` value | 1 | **ng-analyze right.** ngtsc accepts it as an expression. |
| Namespace-member `hostDirectives` (`ns.Dir`) | 0 in the corpus | Found by a probe, not the corpus. Treaty drops the entry silently; ngtsc resolves it. The bridge leaves the name empty (no local binding), so the facade falls back. Covered by `host_directive_names_are_local_bindings` and a parity row (`Treaty defect`). |
| Treaty refuses `deps` with its qualifier first (`[new Optional(), Dep]`) | 1 | **ng-analyze right.** ngtsc reads the markers in any position. |

Treaty defects are kept visible here. They are not hidden by changing Treaty's extractor in the parity
task.

## Golden results with the feature on and off (same moment)

| Bucket | Feature off | Feature on |
| --- | --- | --- |
| Full (`corpus_dump`, compile-verified) | 189 PASS / 0 DIFF of 189 runnable | **189 PASS / 0 DIFF** of 189 runnable |
| Partial (template goldens, `expectEmit`) | 346 runnable, **173 PASS** | 349 runnable, **176 PASS** |

Partial-bucket cases that change outcome: **3 gained** (`should support inline non-literal templates`,
`… using string concatenation`, `… using substitution`: the identifier template is now evaluated, and the
classes compile). **0 lost.** Corpus outputs whose text changed under the feature: 5 (the three templates
above, `property_decorators.ts` and `order_bindings.ts`, where the bridge reads the extra member output and
the decorator `inputs:` array). Those two are still DIFF.

With the feature on, `facts_used` was 733 component and directive classes. Fallbacks (classes that stay on
Treaty's extractor) were counted by reason with `ng_analyze_counters()`: an unresolved external template or
stylesheet (ng-analyze's file-level error, 10 classes; the corpus dump passes no resolved content),
`foreignImports` (a Treaty-only key, 4), and a non-literal `host` value (1).

Facade differential tests (`facade/tests/ng_analyze_facts_path.rs`) compile each snippet twice in one
process, once through the facts path and once with `with_treaty_extraction`: all 10 produce identical code
and errors.

## Scope of the emit path

With the feature on, the facts path emits **components and directives**. Pipes, injectables and NgModules
are still extracted by Treaty's walk, and their facts are produced and checked by the parity test. The
extraction and the emit tail are split so the two extractors feed the same `DirectiveExtract`, and the
feature-off output was checked byte for byte against the pre-refactor facade (identical corpus dump).

## Pinned rev and how to bump it

The git dependency is pinned in `Cargo.toml` to `78d2a67d922b2a2af29d50aed587bc78a99a5450`, the commit
in `tools/render3-sync/ANGULAR_REF`. **Bump both together.** Then:

1. Change `rev` in `libs/treaty-ivy/ng-analyze-bridge/Cargo.toml` and `tools/render3-sync/ANGULAR_REF`.
2. Re-run the parity test and the feature-on golden. Every new explained difference needs a reason in
   `classify` (in the parity test), and any new un-explained one is a finding, not a reason to widen it.
3. Check the oxc and Rust requirements: ng-analyze declares no `rust-version`, and its oxc 0.144 crates
   and `oxc_resolver` 11.24.3 declare `rust-version = "1.95.0"`, the pinned toolchain.

## Blocker: the workspace cannot resolve ng-analyze yet

ng-analyze pulls in `oxc_allocator` 0.144, which needs `hashbrown ^0.17.1`. The workspace's
`libs/runtime` depends on `nova_vm` at rev `bece61ac`, whose manifest pins `hashbrown = "=0.17.0"`. Cargo
locks optional dependencies too, so an optional `ng-analyze` edge anywhere in the workspace makes the whole
workspace fail to resolve, the default build included. Verified: `cargo metadata` fails once the bridge is a
member or a dependency of `treaty_ivy`, and succeeds without it.

The fix is outside this crate: bump `libs/runtime`'s `nova_vm` rev. Upstream nova `main` at
`4eea7c6fae180a8a2eed45c3f6acd0d9256522b1` pins `hashbrown = "=0.17.1"`, which satisfies both oxc versions.
With that rev in a scratch copy and `cargo update -p hashbrown@0.17.0 --precise 0.17.1`, the workspace
resolves with the bridge included. With that rev, `cargo check -p treaty_runtime` passes in the scratch
copy (check only; the runtime's own tests were not run), so the runtime owner still needs to confirm its
behaviour at that rev.

Once that lands, enable the bridge by adding to `libs/treaty-ivy/facade/Cargo.toml`:

```toml
treaty_ivy_ng_bridge = { path = "../ng-analyze-bridge", optional = true }
# in [features]
ng-analyze = ["dep:treaty_ivy_ng_bridge", "treaty_ivy_ng_bridge/analyze"]
```

and adding `libs/treaty-ivy/ng-analyze-bridge` to the root `[workspace] members`. The default and `swc`
features are not changed by either edit.

## Tests

- `cargo test -p treaty_ivy_facts` and `cargo test -p treaty_ivy_ng_bridge`: span conversion (non-ASCII),
  nothing needs ng-analyze.
- `cargo test -p treaty_ivy_ng_bridge --features analyze`: adapter tests on small sources and on the
  assessment fixtures in `tests/fixtures/project` (signal `viewChild.required`, `@Input({required})`,
  cross-file constant selector and options, cross-file directive import, host values, multi-decorator
  classes, non-ASCII spans).
- `cargo test -p treaty_ivy --features ng-analyze --test ng_analyze_parity -- --nocapture` (with
  `NG_ANALYZE_PARITY_REPORT=<path>` for the JSON report) and `--test ng_analyze_facts_path`.
