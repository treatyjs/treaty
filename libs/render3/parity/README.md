# render3 parity-diff harness

`parity.mjs` is an **oracle parity-diff harness**. It compiles a set of fixture
templates with **two** compilers and diffs the resulting Ivy
`ɵɵdefineComponent({...})` output to catch divergences:

1. **Oracle** — `@angular/compiler@21` (`parseTemplate` +
   `compileComponentFromMetadata`), serialised with the same custom output
   printer used by `apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts`
   (ported inline, because `@angular/compiler` exposes no public JS emitter).
2. **Subject** — the Treaty Rust `render3` compiler, reached through the
   `@treaty/authoring-node` NAPI addon:
   `compile_component(template, selector, className) -> { code, errors }`.

It is **independent of the Rust crate** and never edits `libs/render3` source.

## Fixtures

| id                    | template                                          | exercises             |
| --------------------- | ------------------------------------------------- | --------------------- |
| `static-element`      | `<button>Hi</button>`                             | static element/text   |
| `interpolation`       | `<div>{{name}}</div>`                             | text interpolation    |
| `nested`              | `<div><span>{{name}}</span></div>`                | nested elements       |
| `attribute`           | `<div class="box" id="x">y</div>`                 | static attributes     |
| `multiple-bindings`   | `<p>{{a}} and {{b}}</p>`                          | multiple bindings     |
| `property-binding`    | `<div [id]="x"></div>`                            | property binding      |
| `event-binding`       | `<button (click)="f()">go</button>`               | event listener        |
| `two-interpolations`  | `<p>{{a}} {{b}}</p>`                              | two text bindings     |
| `class-binding`       | `<div [class.on]="b">y</div>`                     | class binding         |
| `style-binding`       | `<div [style.color]="c">y</div>`                  | style binding         |
| `control-flow-if`     | `<div>@if (cond) { <span>a</span> }</div>`        | `@if` control flow    |
| `control-flow-for`    | `<ul>@for (x of xs; track x) { <li>{{x}}</li> }</ul>` | `@for` control flow |
| `deep-nesting`        | `<div><p><span>{{x}}</span></p></div>`            | deep element nesting  |
| `sibling-elements`    | `<div></div><span></span>`                        | sibling top-level els |
| `static-and-bound-mix`| `<div class="box" [id]="x">{{t}}</div>`           | static + bound attrs  |
| `control-flow-if-elseif-else` | `<div>@if (a) {…} @else if (b) {…} @else {…}</div>` | `@if`/`@else if`/`@else` chain |
| `control-flow-switch` | `<div>@switch (k) { @case (1) {…} @case (2) {…} @default {…} }</div>` | `@switch`/`@case`/`@default` |
| `nested-for-in-if`    | `<div>@if (cond) { <ul>@for (x of xs; track x) {…}</ul> }</div>` | nested `@for` inside `@if` |
| `multiple-event-bindings` | `<button (click)="f()" (mouseenter)="g()">go</button>` | multiple event listeners |
| `mixed-bindings-element` | `<input [value]="v" (input)="o($event)" [class.err]="e">` | mixed prop/event/class on one el |
| `for-index-count`     | `<ul>@for (x of xs; track x) { <li>{{ $index }} of {{ $count }}: {{x}}</li> }</ul>` | `@for` `$index`/`$count` implicit vars |
| `ng-template-ref`     | `<ng-template #tpl><span>tpl</span></ng-template>` | `ng-template` + ref var |
| `attr-binding`        | `<div [attr.role]="r"></div>`                     | `[attr.*]` binding |
| `pipe-simple`         | `<p>{{ x \| uppercase }}</p>`                     | single pipe in interpolation |
| `pipe-with-args`      | `<p>{{ x \| slice:1:3 }}</p>`                     | pipe with positional args |
| `pipe-chained`        | `<p>{{ x \| uppercase \| lowercase }}</p>`        | chained pipes |
| `pipe-in-binding`     | `<div [title]="t \| uppercase"></div>`            | pipe inside a property binding |

> The Rust `compile_component` feeds `@angular/compiler` **empty** `inputs`/`outputs`
> (it does not scan the template for referenced bindings), so the oracle side uses
> the same empty maps. A `[id]`/`(click)`/`[class.on]`/`[style.color]` binding still
> lowers to its instruction stream without the component declaring an input/output,
> which is exactly what both compilers do — keeping the comparison apples-to-apples.
>
> The same convention extends to the **pipe** fixtures. A `{{ x | name }}` /
> `[p]="x | name"` expression lowers to the pipe instruction stream — `ɵɵpipe(slot,
> 'name')` in the create block and `ɵɵpipeBind1/2/…/ɵɵpipeBindV(slot, …)` in the
> update block — purely from **parsing** the template; both compilers emit those
> instructions whether or not a pipe is *registered*. Pipe declarations (the
> `pipes`/`declarations` map) only feed dependency **resolution** (the def's
> `dependencies` array / standalone-import diagnostics), not the create/update
> instruction stream this harness diffs. The Rust `compile_component` builds its
> metadata with an **empty** `declarations` set and `has_directive_dependencies:
> false` (it does not scan the template to register referenced pipes), so the oracle
> here likewise registers **no** pipes (`declarations: []`,
> `hasDirectiveDependencies: false`) — both sides lower the identical
> `ɵɵpipe`/`ɵɵpipeBindN` stream from the template alone, apples-to-apples.

## Normalisation

Before diffing, both outputs are normalised so cosmetic emitter differences do
not register as divergence:

- unify the import alias (`i0.ɵɵfoo` / `core.ɵɵfoo` → `ɵɵfoo`);
- drop the definition `type:` entry (the oracle omits the TS type the Rust
  emitter may include);
- normalise string quotes (`'x'` → `"x"`);
- strip all whitespace;
- loosely **sort** the argument list of each `(...)` call group, so arg-order
  churn does not mask real divergence.

The report prints **PASS** (normalised outputs identical) or **DIFF** (with the
first divergence index and a windowed snippet of each side).

## Build the NAPI addon (prerequisite for the subject side)

On this machine the documented `napi build` path FAILS (see below), but a plain
per-package cargo build SUCCEEDS. Use the cargo fallback:

```sh
# 1. build just the addon crate (this compiles render3 with the right feature
#    unification; a full-workspace `cargo build` / `napi build` does NOT — it
#    pulls render3 in a configuration that fails to compile, see below)
cargo build -p authoring_node --release

# 2. copy the produced cdylib next to index.js as the platform .node the
#    harness loads (win32-x64 shown; adjust the triple for your platform)
cp target/release/authoring_node.dll \
   libs/authoring/node/authoring_node.win32-x64-msvc.node
```

The harness loads that `.node` **directly** (the committed `index.js`/`index.d.ts`
glue is stale — it only re-exports `sum`, never the compile function). The native
binding exports the compile entrypoint as **`compileComponent`** — napi-rs maps the
Rust `compile_component` to camelCase. The harness accepts either name.

### Why `napi build` fails here

`npx napi build` (and `npx napi`) cannot resolve a `napi` package version
(`npm error code ENOVERSIONS`); the repo-pinned CLI must be run as
`node_modules/.bin/napi`. Even then, `napi build --release` runs a
**full-workspace** `cargo build --release`, which compiles `render3` in a
configuration that currently fails:

```
error[E0599]: no method named `convert_safe` found for `&mut Converter<'_, R>`
   --> libs/render3/src/expression_converter.rs:319 / :332
```

(The older break the previous notes mentioned in `output/emitter.rs` is gone;
this `expression_converter.rs` break only manifests under the whole-workspace
build that `napi build` triggers.) Building just the addon package with
`cargo build -p authoring_node --release` sidesteps it and succeeds. The build
break is in the Rust crate, which this task must not modify.

## Run

```sh
node libs/render3/parity/parity.mjs
```

Exit code: `0` when every comparison passes (or the addon is unavailable and the
harness runs oracle-only), `1` when any fixture **DIFF**s or errors.

## Observed on this machine (2026-05, win32-x64) — BOTH SIDES RAN

The addon built via the cargo fallback and was loaded directly from
`libs/authoring/node/authoring_node.win32-x64-msvc.node`. Both compilers ran for
all 5 fixtures. **Result: 0 PASS, 5 DIFF.** Every fixture diverges. The diffs are
real and consistent — they pinpoint exactly where the Rust `render3` output
trails Angular 21:

| # | divergence | oracle (Angular 21) | Rust render3 | affected fixtures |
|---|------------|---------------------|--------------|-------------------|
| 1 | element instruction name | `ɵɵdomElementStart` / `ɵɵdomElementEnd` | `ɵɵelementStart` / `ɵɵelementEnd` (legacy) | **all 5** |
| 2 | `vars` slot count | real count (`1`, `1`, `2`) | always `0` | interpolation, nested, multiple-bindings |
| 3 | `ɵɵadvance` argument | `ɵɵadvance()` (default 1 omitted) | `ɵɵadvance(1)` / `ɵɵadvance(2)` (explicit) | interpolation, nested, multiple-bindings |
| 4 | static-attr `consts` encoding | `['id','x',1,'box']` — uses `AttributeMarker.Classes` (`1`) so `class="box"` → class-marker entry | `['class','box','id','x']` — plain string attrs, no marker encoding | attribute |
| 5 | `changeDetection` | emits `changeDetection: 0` (OnPush metadata round-trips) | field omitted entirely | all 5 (trailing) |

Divergence #1 is the **first** reported diff for `static-element`, `nested`,
`attribute`; divergences #2/#3 dominate the interpolation fixtures. The `vars: 0`
issue is the documented `R3BoundTarget` binding-slot-counting `NOTE(port)` TODO
in `libs/render3/src/compile.rs`.

Example raw report (first divergence windows):

```
Fixture: static-element  -> DIFF @ index 117
  oracle: ...t_Template(ctx,rf){if(rf&1){ɵɵdomElementStart("button",0);ɵɵ
  rust:   ...t_Template(ctx,rf){if(rf&1){ɵɵelementStart("button",0);ɵɵtex
Fixture: interpolation   -> DIFF @ index 59
  oracle: ...decls:2,vars:1,template:functionHelloCompon
  rust:   ...decls:2,vars:0,template:functionHelloCompon
Fixture: attribute       -> DIFF @ index 70
  oracle: ...consts:[["id","x",1,"box"]],template:fun
  rust:   ...consts:[["class","box","id","x"]],templa
```

These are the actionable signals: the Rust emitter needs the `dom*` instruction
names (Angular 21 rename), real `vars` slot counting, `advance()` default-arg
elision, and `AttributeMarker`-aware `consts` encoding to reach parity.

## Harness fixes applied (parity.mjs only)

1. `loadRustAddon` now loads the native `.node` directly (the committed `index.js`
   glue is stale and only exports `sum`) and accepts the napi-camelCased
   `compileComponent` as well as `compile_component`.
2. `normalize` now strips the `import ... from '...';` preamble the Rust emitter
   prepends (the oracle printer emits only the bare `ɵɵdefineComponent(...)`
   expression), so the two sides align at the definition instead of falsely
   diverging at index 0.
