# Angular → React backend + a runtime-agnostic compiler IR

**Status:** design / plan (not yet implemented)
**Goal:** author a Treaty component once and emit it to *either* Angular Ivy *or* React, by feeding a single normalized IR into pluggable per-runtime emitters. Combined with the existing React→Angular *authoring* lowering, this gives bidirectional, any-runtime mobility: the compiler — not a hand port — moves an app between runtimes.

This doc is deliberately honest about what is already runtime-agnostic in `treaty_ivy` and what is welded to Ivy and must be extracted. It is a refactor plan, not a hand-wave.

---

## 0. Vocabulary and the two distinct "React paths"

There are two completely different directions, and they must not be conflated:

1. **React→Angular (authoring front-end, EXISTS today).** `apps/rust/authoring/src/jsx/react.rs` lowers React *idioms* (`useState`/`useEffect`/`useMemo`/…) into Angular's signal primitives *as a source-to-source pre-pass*, before the normal Angular compile runs. The output is an Angular Ivy component. This is "write React-shaped source, get Angular." It is a front-end normalizer, not a backend.

2. **Angular→React (backend, THIS doc).** Take a fully-parsed Angular component (the `treaty_ivy` IR: template `r3_ast` + binding `ExprKind` + `R3ComponentMetadata`) and emit a React `.tsx` file. This is a *new emitter* sitting where the Ivy emitter sits today.

The unifying idea: there is one IR in the middle. Path (1) feeds it; the Ivy emitter and a new React emitter consume it. Path (2) is the React emitter. Once both emitters exist, any authoring front-end (`.treaty`, JSX, raw `@Component`) can target any runtime.

```
                       authoring front-ends                       backend emitters
  .treaty ─┐
  JSX/.tsx ─┤──► [ normalize ] ──►  TREATY COMPONENT IR  ──►──┬──► Ivy emitter   ──► Angular .js  (TODAY)
  @Component┘   (incl. react.rs                                └──► React emitter ──► React .tsx   (NEW)
                 React→Angular
                 pre-pass)
```

---

## 1. Architecture: a runtime-agnostic IR with pluggable emitters

### 1.1 What the IR is (and what already exists)

A Treaty component decomposes into three sub-IRs. The good news: **two of the three are already runtime-agnostic in `treaty_ivy` today.** The coupling is concentrated in exactly one place.

| Sub-IR | What it describes | Where it lives today | Runtime-agnostic today? |
| --- | --- | --- | --- |
| **Binding-expression IR** | the JS expressions inside `{{…}}`, `[x]="…"`, `(e)="…"` — `PropertyRead`, `Call`, `Conditional`, `Binary`, `Interpolation`, `BindingPipe`, `ArrowFunction`, … | `libs/treaty-ivy/core/src/expression/ast.rs` (`ExprKind` / `AstNode`) | **YES.** This is a plain JS-expression AST. It mentions nothing Ivy. |
| **Template IR** | the desugared element/control-flow tree — `Element`, `BoundAttribute`, `BoundEvent`, `IfBlock`, `ForLoopBlock`, `SwitchBlock`, `Content`, `Template`, `LetDeclaration`, … | `libs/treaty-ivy/template/src/template/r3_ast.rs` (`Node`) | **MOSTLY.** `r3_ast::Node` describes *what* (an if/for/switch with branches), not *how* (no `ɵɵ` instruction is encoded in the node). It is Angular-*named* but instruction-free. Caveat: `Content` (ng-content), `Reference` (`#ref`), `@let`, pipes and a few node fields carry Angular semantics that need an explicit React story (see §2). |
| **Reactive/class IR** | the component class: state (`signal`), derived (`computed`), effects (`effect`), inputs/outputs, lifecycle, queries, host bindings/listeners, plain methods/fields | spread across `R3ComponentMetadata` / `R3DirectiveMetadata` (`libs/treaty-ivy/decorators/src/compiler.rs`) + the raw class body JS the front-end passes through | **PARTIALLY.** The *metadata* (inputs/outputs/host/lifecycle/queries) is structured and agnostic. The *body* (method bodies, signal declarations) is currently carried as opaque JS text destined for an Angular class. That text assumes `this.x()`/`this.x.set(…)` Angular signal semantics and must be normalized (see §1.3, §4). |

So the architecture is **not** "build a brand-new IR from scratch." It is: **promote the existing `expression::ast` + `r3_ast` to the public, target-neutral IR, and extract the one Ivy-coupled stage into a pluggable emitter.**

### 1.2 Where the Ivy coupling actually is (the honest part)

The single concentrated coupling point is **`libs/treaty-ivy/template/src/view/template.rs`** (the `TemplateDefinitionBuilder`, ~475 KB). This is the stage that walks `r3_ast::Node` and emits the Ivy creation/update instruction streams (`ɵɵelementStart`, `ɵɵtext`, `ɵɵproperty`, `ɵɵadvance`, `ɵɵtextInterpolate1…8`, `ɵɵlistener`, `ɵɵif`, `ɵɵfor`, …) as `output_ast` statements. Everything Ivy-specific — slot allocation, the constant pool, `ɵɵnextContext()` hops, `ɵɵreference` slots, `ɵɵpipeBindN` var offsets — lives here.

The decorator orchestrator (`libs/treaty-ivy/decorators/src/compiler.rs`) already hides this behind a trait:

```rust
// libs/treaty-ivy/decorators/src/compiler.rs
pub trait TemplateBuilder {
    fn build<D: R3TemplateDependency>(
        &mut self,
        meta: &R3ComponentMetadata<D>,
        all_deferrable_deps_fn: Option<&Expr>,
        deferred_deps: &HashMap<(u32, u32), Expr>,
    ) -> TemplateBuilderResult;
}
```

`RealTemplateBuilder` (in the facade crate) is the Ivy implementation; `StubTemplateBuilder` is the test stub. **This trait is the seam — but it is not yet a runtime-agnostic seam**, because `TemplateBuilderResult` is Ivy-shaped:

```rust
pub struct TemplateBuilderResult {
    pub template_fn: Expr,        // `function MyComponent_Template(rf, ctx) {…}` — Ivy view fn
    pub decls: u32,               // Ivy slot count
    pub vars: u32,                // Ivy binding-slot count
    pub consts: Vec<Expr>,        // Ivy consts array
    pub consts_initializers: Vec<Stmt>,
    pub content_selectors: Option<Expr>,  // Ivy ngContentSelectors
    pub pool_statements: Vec<Stmt>,        // hoisted Ivy nested-view fns
}
```

`decls`/`vars`/`consts`/`pool_statements` are meaningless for React. So `TemplateBuilder` abstracts *who builds the Ivy template fn*, not *which runtime we target*. **The realistic refactor is to introduce a target seam one level up.**

### 1.3 The realistic refactor (the extraction)

Three concrete moves, smallest-blast-radius first:

**Move A — publish the neutral IR as a crate boundary.** `expression::ast` and `r3_ast` are already owned/arena-free. Re-export them from a new `treaty-ivy/ir` crate (or a `pub mod ir` in `core`) as **the** public component IR, with a documented contract: "no consumer may assume an Ivy target." `R3ComponentMetadata` is mostly reusable but is named/shaped for Ivy (`R3TemplateDependency`, `ngContentSelectors`, `DeferBlockDepsEmitMode`); extract a leaner `ComponentModel { name, inputs, outputs, host, lifecycle, queries, template: Vec<ir::Node>, reactive_decls, body }` that `R3ComponentMetadata` is *derived from* for the Ivy path. This is the keystone deliverable and the only data-model change.

**Move B — normalize the class body into a Reactive IR instead of passing opaque Angular JS.** Today the method bodies travel as text that already says `this.todos()` / `this.inputValue.set(...)`. For a React target we must know which identifiers are signals, computeds, refs, inputs — *as structured facts*, not as text we string-rewrite. Two sub-options:
  - **B1 (fast, lossy-tolerant):** reuse the *exact same span-edit machinery* `react.rs` already has, but in reverse — a body-rewrite pass keyed off the reactive-declaration set (`signal`→`useState`, `x.set(v)`→`setX(v)`, `x.update(fn)`→`setX(fn)`, `x()`→`x`, `computed`→`useMemo`, `effect`→`useEffect`). This is mechanically the mirror of `react.rs`'s Pass-1/Pass-2 two-pass design and can ship first.
  - **B2 (correct, eventual):** lower method bodies into `output_ast::Stmt`/`Expr` (which already model `VariableDeclaration`, `CallExpression`, `ArrowFunction`, `ConditionalExpr`, etc., per `core/src/output_ast.rs`) so the React emitter renders from structured statements, not text. B2 is the long-term home; B1 unblocks the worked example in §5 immediately.

**Move C — add a `RuntimeEmitter` trait above `TemplateBuilder`.** This is the new pluggable seam:

```rust
pub trait RuntimeEmitter {
    /// Emit a whole component (template + reactivity + metadata) to a runtime's source string,
    /// plus a v3 source map anchoring output back to authoring source.
    fn emit_component(&mut self, model: &ComponentModel) -> EmittedModule;
}

pub struct EmittedModule { pub code: String, pub source_map: SourceMap, pub diagnostics: Vec<Diag> }
```

- `IvyEmitter` wraps today's `compile_component_from_metadata` + `RealTemplateBuilder` path verbatim. **Zero behavior change** for the Angular target — it just gets a name and an interface.
- `ReactEmitter` is new (§2–§5). It walks `ir::Node` to emit JSX, walks the Reactive IR to emit hooks, and renders binding `ExprKind` to JS expressions (the expression printer is shared — `ExprKind` → JS is runtime-neutral and already exists conceptually in the emitter's `emit_expression`).

**What this refactor does NOT require:** rewriting `template.rs`. The Ivy instruction emitter stays exactly as-is, owned by `IvyEmitter`. The React emitter is a *sibling* consumer of the same `r3_ast`, not a modification of the Ivy lowering.

### 1.4 Shared vs. per-runtime, concretely

| Component | Shared across runtimes | Ivy-only | React-only |
| --- | --- | --- | --- |
| HTML/template parse → `r3_ast` (`ml_parser.rs`, `control_flow.rs`, `template_transform.rs`) | ✅ | | |
| Binding expression parse → `ExprKind` (`expression/parser.rs`, `ast.rs`) | ✅ | | |
| Expression printer `ExprKind`→JS (`emit_expression`) | ✅ (mostly; pipes/refs differ — §2) | | |
| `r3_ast` → instruction stream (`view/template.rs`) | | ✅ | |
| `r3_ast` → JSX | | | ✅ |
| Reactive decls → `signal()`/`computed()`/`effect()` | | ✅ | |
| Reactive decls → `useState`/`useMemo`/`useEffect` | | | ✅ |
| Host bindings → `hostBindings` fn (`compiler.rs`) | | ✅ | |
| Host bindings → root-element props (§3) | | | ✅ |

The ✅-shared column is the bulk of the parser/transform code and is already written and tested. That is why this is feasible rather than a rewrite.

---

## 2. Angular → React construct mapping table

Difficulty: **trivial** = mechanical 1:1; **moderate** = needs a chosen pattern + some glue; **hard** = semantically lossy or no clean equivalent (surface a diagnostic, don't silently mis-emit — the same "don't block, surface the gap" contract `react.rs` uses for `useReducer`).

### Components, reactivity, control flow (mostly trivial)

| Angular | React | Difficulty | Notes |
| --- | --- | --- | --- |
| `@Component` (template + class) | function component returning JSX | trivial | class body becomes function body; template becomes the returned JSX |
| `signal(init)` | `const [x, setX] = useState(init)` | trivial | also escape hatch via `@preact/signals-react` (§4) |
| `computed(fn)` | `useMemo(fn, [deps])` | trivial→moderate | `computed` auto-tracks; `useMemo` needs an explicit deps array we must infer (§4) |
| `effect(fn)` | `useEffect(fn, [deps])` | trivial→moderate | same deps-inference concern as `computed` |
| `@if` / `*ngIf` | `{cond && <…/>}` or `{cond ? <A/> : <B/>}` | trivial | `@else if`/`@else` chain → nested ternaries |
| `@for (x of xs; track t)` / `*ngFor` | `{xs.map((x) => <… key={t}/>)}` | trivial→moderate | `track` → `key`; `$index`/`$first`/`$last`/`$even`/`$odd` → `.map((x, i, arr) => …)` derived locals; `@empty` → `xs.length ? … : <empty/>` |
| `@switch`/`@case`/`@default` / `*ngSwitch` | nested ternary, or IIFE `switch`, or object map | moderate | ternary chain is simplest; preserve fall-through-free Angular semantics |
| `{{ value }}` | `{value}` | trivial | |
| `[prop]="value"` | `prop={value}` | trivial | |
| `(click)="handler($event)"` | `onClick={handler}` / `onClick={(e) => handler(e)}` | trivial | `$event` → `e`; React synthetic event |
| `[(ngModel)]="v"` | `value={v} onChange={(e)=>setV(e.target.value)}` | moderate | desugar two-way into value + change |
| `@Input()` / `input()` | destructured props `function C({prop})` | trivial | required vs optional → TS prop types |
| `@Output()` / `output()` | callback props `onEvent` | trivial | `emit(x)` → `props.onEvent?.(x)` |
| `<ng-content>` | `{props.children}` | moderate | see lossy note below |
| `<ng-content select="x">` | named slot prop `{props.x}` | moderate | multi-slot projection has no native React equivalent; map to named render props |
| `ngOnInit` | `useEffect(() => {…}, [])` | trivial | |
| `ngOnDestroy` | `useEffect(() => () => {…}, [])` | trivial | cleanup return |
| `ngOnChanges` | `useEffect(() => {…}, [a, b])` | moderate | needs `usePrevious` for prev-vs-current |
| `ngAfterViewInit` | `useEffect(() => {…}, [])` after ref attach | moderate | ordering caveats |
| pipes `{{ v | uppercase \| slice:0:5 }}` | function calls / `useMemo` | trivial→moderate | builtin pipes → JS (`toUpperCase`, `.slice`); custom pipes → imported pure fn; `async` pipe is **hard** (RxJS, see below) |
| `@ViewChild`/`@ViewChildren` | `useRef` / `forwardRef` | moderate | |
| `@ContentChild`/`@ContentChildren` | `React.Children` utils / refs on children | moderate | |
| OnPush change detection | `React.memo(Component)` | moderate | wrap export |
| DI / `inject(Service)` | `useContext(Ctx)` + provider | moderate | Angular injector is implicit; React needs explicit `<Provider>` nesting + a wrapper hook |
| singleton service | context + custom hook, or Zustand/Jotai | moderate | |
| `Router` / `ActivatedRoute` | `react-router-dom` (`useNavigate`, `useParams`) | moderate | library mapping, not 1:1 |
| Reactive Forms | `react-hook-form` (+ Zod) | hard | no structural equivalent; library translation |

### Lossy / no-equivalent (must emit a diagnostic, may emit a best-effort shim)

| Angular | React | Difficulty | Why it's lossy |
| --- | --- | --- | --- |
| RxJS `Observable`/`Subject`/`switchMap`/`async` pipe | `useState`+`useEffect`, or TanStack Query, or `@react-rxjs/core` | **hard** | RxJS is a whole reactive runtime; no mechanical mapping. Emit a `// TODO(treaty): RxJS …` shim + diagnostic. |
| `useReducer`-shaped state machines round-tripped | — | hard | mirror of `react.rs`'s own `useReducer` gap; left in place with a diagnostic |
| ViewEncapsulation `Emulated` (`%COMP%`/`_ngcontent`) | CSS Modules / styled-components / inline | moderate→hard | known gap (`component-styles-and-routerlink-bugs` memo): Treaty does not yet emulate scoping even for Ivy. React target should emit a CSS Module (`.module.css`) and `className={styles.x}` rather than global CSS. |
| `ng-template` + `*ngTemplateOutlet` / `TemplateRef` | render-prop function / `React.ReactNode` | moderate→hard | `TemplateRef`/`ViewContainerRef` imperative insertion has no clean React analog; render props cover the common cases |
| multi-slot `ng-content` selectors | named props | moderate | React has no slot selector matching; we choose a naming convention |
| content/host queries returning live `QueryList` | static ref arrays | moderate | `QueryList` is observable/live; React refs are snapshot |
| `@HostBinding`/`@HostListener` | root-element props (§3) | moderate | fine for components; for *directives* it's the smart-directive problem (§3) |

---

## 3. The smart directive strategy

Angular directives have no first-class React counterpart. The naive "stub it out" approach (called out as a bug in the `server-fn-leak-and-stub-bugs` memo — a real directive was stubbed in `counter.tsx`) is exactly what we must avoid. The strategy below picks the *idiomatic React shape per directive kind*, and it composes with how Treaty *already authors* directives in JSX (`apps/rust/authoring/src/jsx/directives.rs` recognizes attribute and `*`-structural forms today).

### 3.1 Attribute directive → custom hook (default) or wrapper component

An attribute directive (`appHighlight`, `Tooltip`, `use:autofocus`) is reusable host behavior: host bindings + host listeners + lifecycle, no own view. The idiomatic React form is a **custom hook that returns the props to spread onto the host element**:

```ts
// Angular directive (host bindings + listener)
@Directive({ selector: '[appHighlight]' })
class Highlight {
  @Input() color = 'yellow';
  @HostBinding('style.backgroundColor') bg = '';
  @HostListener('mouseenter') on()  { this.bg = this.color; }
  @HostListener('mouseleave') off() { this.bg = ''; }
}
```

emits:

```tsx
// React: a hook returning host props to spread
function useHighlight(color = 'yellow') {
  const [bg, setBg] = useState('');
  return {
    style: { backgroundColor: bg },
    onMouseEnter: () => setBg(color),
    onMouseLeave: () => setBg(''),
  };
}
// usage at the application site: <span {...useHighlight('cyan')}>…</span>
```

- `@HostBinding('style.x')`/`('class.x')`/`('attr.x')` → keys of the returned props object (`style`, `className`, attribute name).
- `@HostListener('event')` → `onEvent` keys in the returned object.
- `@Input()` → hook parameters.
- Inputs that are signals stay signals (`useState`) inside the hook.
- **Default = hook.** A **wrapper component** (`<Highlight>…</Highlight>` that clones its child and injects props) is the fallback when the directive must also project/own children or when spreading onto an unknown element is unsafe.

This is exactly what the research table recommends ("custom hook that returns an object of props/handlers, or wrapper component"), and it lines up with Treaty's existing directive *authoring* model where a directive is "the same shape minus the view, returns a host spec" (`jsx-treaty-directive-authoring` memo) — so the React emitter is reading a host-spec it already understands.

### 3.2 Structural directive → render-prop helper component (default) or HOC

A structural directive (`*appUnless`, `*appRepeat`, `*ngIf`-like) controls *whether/how often* a subtree renders. The Angular template `<div *appUnless="cond">…</div>` already desugars (in `r3_ast`, via `control_flow.rs` / the structural-directive lowering in `template_transform.rs`) into a `Template` node wrapping the host with the directive bound. The React emitter renders that as a **render-prop component**:

```tsx
// Angular: <div *appUnless="hideIt">secret</div>
// React:
<Unless cond={hideIt}>{() => <div>secret</div>}</Unless>

function Unless({ cond, children }: { cond: boolean; children: () => React.ReactNode }) {
  return cond ? null : <>{children()}</>;
}
```

- The wrapped subtree becomes the render-prop child (`() => <…/>`), so it is only evaluated when the directive decides to render it (faithful to structural semantics — no eager evaluation).
- Directives that introduce template locals (`*ngFor`'s `let item`, `index`) map to render-prop *parameters*: `{(item, i) => <…/>}`.
- **Default = render-prop component.** A **HOC** (`withUnless(Component)`) is the fallback when the structural directive wraps a whole component rather than an inline subtree.
- Treaty already recognizes the `*highlight` / `structural:highlight` authoring forms (`directives.rs`), so on the *emit* side we have the structural marker in the IR and just choose the render-prop shape.

### 3.3 Host bindings/listeners on a *component* (not a directive)

When `@HostBinding`/`@HostListener` appear on a *component* (the component's own host element), there is no separate hook to extract — they bind to the **root JSX element** the component returns:

```tsx
// @HostBinding('class.active') / @HostListener('click')
function Panel(props) {
  const [active, setActive] = useState(false);
  return (
    <div className={cx({ active })} onClick={() => setActive(a => !a)}>
      {/* …template… */}
    </div>
  );
}
```

- If the component's template has a single root element, merge host bindings/listeners onto it.
- If the template has multiple roots or text roots, wrap in a fragment-less single root is impossible — emit a wrapping `<div>` **only** when host bindings exist (and flag it as a structural change in diagnostics, since it alters the DOM).
- Global `@HostListener('window:resize')` / `('document:click')` → `useEffect(() => { window.addEventListener(...); return () => window.removeEventListener(...); }, [])`.

### 3.4 Resolution + the "smart" decision table

The emitter classifies each directive from the IR and picks the shape:

| Directive shape (from IR) | React emission | Fallback |
| --- | --- | --- |
| attribute, host bindings/listeners, no view | custom hook returning props | wrapper component |
| structural (`*`), wraps inline subtree | render-prop component | — |
| structural, wraps whole component | render-prop component | HOC |
| host bindings/listeners on the component itself | root-element props | wrapping `<div>` (+ diagnostic) |
| directive with its own template/projection | wrapper component | — |
| unresolved / opaque directive | **diagnostic, no stub** | emit `// TODO(treaty): directive <name> not lowerable` + keep call visible |

The hard rule (from the stub-bug memo): **never silently stub a directive to a no-op.** If we can't classify it, we emit a visible TODO + a diagnostic, never a fake passthrough.

---

## 4. Reactivity strategy (signals → React)

### 4.1 The default: `useState` + inferred-deps `useMemo`/`useEffect`

The **default target is plain React hooks**, no runtime dependency. This is the exact inverse of what `react.rs` already does (React→Angular), so the mapping table is reused verbatim in reverse:

| Angular (IR) | React (default) |
| --- | --- |
| `const x = signal(init)` | `const [x, setX] = useState(init)` |
| read `x()` | `x` |
| `x.set(v)` | `setX(v)` |
| `x.update(fn)` | `setX(fn)` |
| `const m = computed(fn)` | `const m = useMemo(fn, [deps])` |
| `effect(fn)` | `useEffect(fn, [deps])` |
| `input()` / `@Input()` | destructured prop |
| `inject(C)` | `useContext(C)` |
| `viewChild`/`@ViewChild` | `useRef` (+ `forwardRef` on the child) |

**The one genuinely new problem vs. `react.rs`:** `react.rs` could *drop* deps arrays going React→Angular (Angular's `computed`/`effect` auto-track). Going Angular→React we must *synthesize* a deps array, because `useMemo`/`useEffect` do not auto-track. The emitter infers deps by walking the `computed`/`effect` body (it is already an `ExprKind`/`output_ast` tree once Move B2 lands) and collecting the free identifiers that are reactive (signals, computeds, inputs, props). This is a static approximation:
- It is *conservative-correct* for the common cases (read `x()` inside the body → `x` is a dep).
- Where it can't prove the dep set (dynamic indexing, conditional reads behind opaque calls), it emits the deps it found **plus an `// eslint-disable-next-line react-hooks/exhaustive-deps` and a diagnostic**, rather than silently producing a stale closure.

### 4.2 The escape hatch: `@preact/signals-react`

For components where (a) deps inference is lossy, (b) fine-grained reactivity is wanted, or (c) the author opts in via a flag, the emitter targets **`@preact/signals-react`** instead, which gives `signal`/`computed`/`effect` with auto-tracking — a near-1:1 mapping that *avoids the deps-array problem entirely*:

| Angular (IR) | React + `@preact/signals-react` |
| --- | --- |
| `const x = signal(init)` | `const x = useSignal(init)` |
| read `x()` | `x.value` |
| `x.set(v)` | `x.value = v` |
| `const m = computed(fn)` | `const m = useComputed(fn)` (auto-tracks, no deps) |
| `effect(fn)` | `useSignalEffect(fn)` (auto-tracks, no deps) |

This is the higher-fidelity path and is the recommended default *when the dependency is acceptable*. Policy: **`useState` default for zero-dependency portability; `@preact/signals-react` when a `computed`/`effect` body fails deps-inference or when `--react-signals` is set.** Choose per-component, recorded in diagnostics so the output is explainable.

---

## 5. Worked example

Source: `apps/repl/src/gallery/samples/angular-component.ts` (the `TodoItemComponent` — standalone, signals + `computed`, `@if`/`@for`, `(input)`/`(click)`/`(change)` handlers, `[value]`/`[checked]`/`[class.completed]` bindings, inline `styles`).

Below is the **target output the React emitter must produce** — hand-written to specify the contract. It uses the default `useState` strategy (§4.1) and a CSS Module for styles (§2 lossy note).

```tsx
// TodoItem.tsx  — emitted by ReactEmitter from the same IR the Ivy emitter consumes.
import { useState, useMemo } from 'react';
import styles from './TodoItem.module.css';

type Todo = { text: string; completed: boolean };

export default function TodoItem() {
  // signal<Todo[]>([])            → useState
  const [todos, setTodos] = useState<Todo[]>([]);
  // signal('')                    → useState
  const [inputValue, setInputValue] = useState('');

  // computed(() => todos().filter(t => t.completed).length)
  //   read todos() → `todos`; deps inferred = [todos]
  const completed = useMemo(
    () => todos.filter((todo) => todo.completed).length,
    [todos],
  );

  // addTodo(): this.todos.update(...) → setTodos(...) ; this.inputValue.set('') → setInputValue('')
  const addTodo = () => {
    const text = inputValue.trim();
    if (text) {
      setTodos((todos) => [...todos, { text, completed: false }]);
      setInputValue('');
    }
  };

  // toggleTodo(todo) — update → setTodos(fn)
  const toggleTodo = (todo: Todo) => {
    setTodos((todos) =>
      todos.map((t) => (t === todo ? { ...t, completed: !t.completed } : t)),
    );
  };

  // updateInput(event) — set → setInputValue
  const updateInput = (event: React.ChangeEvent<HTMLInputElement>) => {
    setInputValue(event.target.value);
  };

  return (
    <section className={styles.todoItem}>
      <h2>Todo Counter</h2>

      <div className={styles.inputGroup}>
        {/* [value]="inputValue()" → value={inputValue} ; (input) → onInput */}
        <input
          type="text"
          value={inputValue}
          onInput={updateInput}
          placeholder="Add a todo"
        />
        {/* (click)="addTodo()" → onClick={addTodo} */}
        <button type="button" onClick={addTodo}>Add</button>
      </div>

      {/* @if (todos().length > 0) { … } @else { … }  →  ternary */}
      {todos.length > 0 ? (
        <>
          <ul className={styles.todoList}>
            {/* @for (todo of todos(); track todo) → todos.map, track → key */}
            {todos.map((todo, i) => (
              {/* [class.completed]="todo.completed" → conditional className */}
              <li key={i} className={todo.completed ? styles.completed : undefined}>
                {/* [checked] → checked ; (change) → onChange */}
                <input
                  type="checkbox"
                  checked={todo.completed}
                  onChange={() => toggleTodo(todo)}
                />
                <span>{todo.text}</span>
              </li>
            ))}
          </ul>
          {/* {{ completed() }} of {{ todos().length }} → {completed} of {todos.length} */}
          <p className={styles.status}>{completed} of {todos.length} completed</p>
        </>
      ) : (
        <p className={styles.empty}>No todos yet. Add one to get started!</p>
      )}
    </section>
  );
}
```

with the styles lifted to `TodoItem.module.css` (camelCased class keys; `li.completed span` becomes a scoped selector). Notes the emitter must honor, all visible in this example:

1. **Signal read → value drop:** every `todos()` / `inputValue()` / `completed()` becomes the bare identifier. This is the inverse of `react.rs`'s body auto-call pass (which adds `()`); here we *strip* it.
2. **`@for ... track todo`:** `track`'s expression becomes `key`. Where `track` is the item itself (not a stable id), the emitter falls back to the index `i` and emits a diagnostic (Angular allows object-identity tracking; React wants a stable key — a real fidelity caveat to surface, not hide).
3. **`@if/@else` → ternary** with a fragment wrapping the multi-node `@if` body.
4. **`[class.x]` → conditional `className`**; multiple `[class.*]` would compose via a `cx`/`clsx` helper.
5. **`(input)`/`(change)`/`(click)` → `onInput`/`onChange`/`onClick`**, `$event`→the handler's event param.
6. **`this.` is dropped** (class → function scope) and method fields become `const` arrows (so they are stable closures over state).
7. **Styles** are a known lossy area — emitted as a CSS Module to get scoping React-idiomatically, since Treaty does not emulate Angular's `%COMP%` scoping anyway.

This is the spec for `ReactEmitter::emit_component`: given the `TodoItemComponent` IR, produce this `.tsx` + map.

---

## 6. Phased rollout

The harness contract: **every phase is gated by a runtime-behavior parity test, not by string matching** (the `no-regex-verification` memo is binding — we parse/run output, never regex it).

### Phase 0 — extract the seam (no new target yet)
- Move A: publish `expression::ast` + `r3_ast` as the public neutral IR; carve `ComponentModel` out of `R3ComponentMetadata`.
- Move C: add the `RuntimeEmitter` trait; wrap today's Ivy path as `IvyEmitter`.
- **Gate:** the entire existing Ivy golden + compile suite (the 594 compile / matchGolden set in the `render3-port-state` memo, plus `libs/treaty-ivy/facade/tests/link_real_packages.rs`) is **byte-identical** before/after. This phase ships only if Angular output does not move.

### Phase 1 — React emitter, "trivial" constructs only
- `@Component` skeleton, `useState`/`useMemo`/`useEffect`, `@if`/`@for`/`@switch`, interpolation, property/event bindings, `@Input`/`@Output`, lifecycle hooks.
- Target the `TodoItemComponent` worked example as the first golden.
- **Gate (parity harness):** render the *emitted React* and the *emitted Angular* from the same IR in headless DOM, drive the same script of events (type, click Add, toggle), assert identical resulting DOM/text. Reuse the e2e harness style already in `examples/linker-smoke/e2e.mjs`. A component passes Phase 1 only when React and Ivy produce the same observable behavior.

### Phase 2 — directives (the smart part, §3)
- Attribute → hook, structural → render-prop, host bindings → root props.
- **Gate:** directive behavior parity (mouseenter highlight, structural show/hide) under the same DOM-driving harness; plus a "no silent stub" lint that fails the build if any directive emitted a no-op passthrough.

### Phase 3 — moderate constructs
- DI→context, `[(ngModel)]`, `ViewChild`/`ContentChild`, OnPush→`memo`, pipes (builtin + custom), `ng-content`→`children`/named slots, ViewEncapsulation→CSS Modules.
- **Gate:** per-construct parity goldens; each lossy mapping must emit its diagnostic and be listed in a `LOSSY.md` ledger.

### Phase 4 — hard / library-backed + the deps-inference hardening
- Router→`react-router`, Reactive Forms→`react-hook-form`, RxJS→TanStack Query/`@react-rxjs`, `@preact/signals-react` escape hatch (§4.2).
- Harden `computed`/`effect` deps inference (Move B2): switch from text rewrite to `output_ast`-based emission so deps are derived from the real expression tree.
- **Gate:** these are explicitly best-effort + diagnostic-gated; parity tests where a faithful mapping exists, diagnostics + manual-review markers where it does not.

### 6.1 Composing into bidirectional, any-runtime mobility
- **React→Angular** already exists (`react.rs`, front-end). **Angular→React** is this plan (backend emitter).
- Once both exist, the round trip is testable end-to-end: author React → `react.rs` normalizes to Angular IR → `IvyEmitter` (today) *and* `ReactEmitter` (new). A **round-trip parity gate** (React source → IR → React emit → behavior-equal to the original) catches regressions in either direction and proves the IR is the real pivot, not a lossy waypoint.
- The same IR + `RuntimeEmitter` seam is where a future Vue/Svelte/Solid emitter plugs in (the `treaty-macro-isolate-runtime` / authoring-plugin direction), making "author once, target any runtime" a matter of adding emitters, not forking the compiler.

---

## 7. Files this plan touches (real paths)

- **IR (promote to public, neutral):** `libs/treaty-ivy/core/src/expression/ast.rs` (`ExprKind`), `libs/treaty-ivy/template/src/template/r3_ast.rs` (`Node`), `libs/treaty-ivy/core/src/output_ast.rs` (`Stmt`/`Expr` for body lowering).
- **Seam to add (`RuntimeEmitter`):** above `libs/treaty-ivy/decorators/src/compiler.rs`'s `TemplateBuilder`/`compile_component_from_metadata`; new `ComponentModel` derived from `R3ComponentMetadata`.
- **Ivy emitter (wrap, do not change):** `libs/treaty-ivy/core/src/output/emitter.rs`, `libs/treaty-ivy/template/src/view/template.rs`, `libs/treaty-ivy/facade/src/compile.rs` (`RealTemplateBuilder`).
- **New `ReactEmitter`:** new module under `libs/treaty-ivy/` consuming `r3_ast` + `ExprKind` + `ComponentModel`.
- **Body normalization (mirror of React→Angular):** model on `apps/rust/authoring/src/jsx/react.rs` (two-pass, right-to-left span edits) for Move B1; graduate to `output_ast` for B2.
- **Directive classification source-of-truth:** `apps/rust/authoring/src/jsx/directives.rs` (attribute/`*`-structural recognition) + `R3DirectiveMetadata`/`R3HostMetadata` in `compiler.rs`.
- **Parity harness:** extend the style of `examples/linker-smoke/e2e.mjs`; first golden = `apps/repl/src/gallery/samples/angular-component.ts`.
