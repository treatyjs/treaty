# SWC backend plan — a second parser/codegen backend kept 1:1 with OXC

Status: design / not yet implemented
Owner: compiler core
Related: `migration/RENDER3-SYNC-PLAN.md`, `migration/PORT-ARCHITECTURE.md`,
`libs/treaty-ivy/core/src/output_ast.rs`, `libs/treaty-ivy/core/src/output/emitter.rs`

## 0. Goal and non-goals

**Goal.** Let Treaty pick the JS/TS *parser + codegen* backend that best fits the host build
system, while emitting **byte-identical Ivy** either way:

- **OXC** (default) — for rolldown / oxc-native pipelines (the rolldown bundler, oxc-resolver,
  `@oxc-project/*` toolchains). This is what ships today and stays the default.
- **SWC** — for hosts whose toolchain already embeds SWC (Next.js / Turbopack-style stacks,
  `@swc/core` Jest transforms, Nx executors built on `@swc-node`), so Treaty can plug into the
  parser/printer the host already loads instead of dragging a second engine into the process.

The backend choice must NEVER change the compiled output. The same `@Component` source, the same
partial-declaration module, and the same JSX/`.treaty` SFC must produce the same
`ɵɵdefineComponent({...})` / `ɵɵdefine*` text under both backends. Backend selection is a
*performance / host-integration* decision, not a semantic one.

**Non-goals.**

- Not replacing OXC. OXC stays the default and the reference.
- Not abstracting *semantic analysis*. Treaty does not depend on `oxc_semantic` in the Ivy hot path
  today (see §2, the "hard" rows), and we will not require an SWC equivalent — the abstraction is
  drawn so semantic analysis stays on the OXC side of the line if it is ever added.
- Not a runtime concern. This is purely a compile-time, build-host integration switch.

## 1. Where OXC actually touches the compiler (the seam we are abstracting)

OXC is used in exactly two roles, and they sit at the two ends of the pipeline. The middle — the
entire Ivy compiler — is **already backend-neutral** because it operates on Treaty's *own* owned IR
(`output_ast`), not on any oxc AST.

```
  source text                                       Ivy JS text
       │                                                 ▲
       ▼                                                 │
  ┌─────────┐   oxc_ast::Program   ┌──────────────┐   ┌──────────┐
  │ PARSE   │ ───────────────────▶ │ front-end    │   │  EMIT    │
  │ (oxc)   │                      │ extract meta │   │ (oxc)    │
  └─────────┘                      └──────┬───────┘   └────▲─────┘
   role A: parse                          │                │
                                          ▼                │
                            R3*Metadata ──▶ treaty_ivy ──▶ output_ast (o::Stmt / o::Expr)
                                              (NO oxc types — owned IR; backend-neutral)
```

- **Role A — PARSE (text → AST → metadata).** Front-ends parse a TS/TSX string and walk the AST to
  pull out `R3ComponentMetadata` / declaration objects. Real call sites:
  - `libs/treaty-ivy/facade/src/source_compile.rs` (`@Component`/`@Directive` source front-end) —
    `oxc_parser::Parser::new(&alloc, src, SourceType::ts()).parse()`, then walks
    `Class`/`Decorator`/`ObjectExpression`.
  - `libs/treaty-ivy/facade/src/linker.rs` (partial-declaration linker) — parses the published
    library module, finds each `ɵɵngDeclare*({...})` `CallExpression`, reads its `ObjectExpression`
    into `R3*Metadata`.
  - `apps/rust/authoring/src/angular_source.rs`, `apps/rust/authoring/src/jsx/*.rs`
    (`ts_erase.rs`, `control_flow.rs`, `react.rs`, `signals.rs`, `template.rs`, `mod.rs`),
    `apps/rust/authoring/src/plugin/mod.rs` — JSX / `.treaty` authoring front-ends.

- **Role B — EMIT (output_ast → AST → text).** Exactly ONE chokepoint:
  `libs/treaty-ivy/core/src/output/emitter.rs`. `Lowerer` lowers `o::Stmt`/`o::Expr` into an
  `oxc_ast::Program` via `oxc_ast::AstBuilder`, then `oxc_codegen::Codegen` prints it. Public
  surface is just four functions:
  - `emit_statements(&[o::Stmt]) -> String`
  - `emit_expression(&o::Expr) -> String`
  - `emit_statements_with_map(...)` / `emit_expression_with_map(...)` (add v3 source maps)

  `treaty_ivy` and the front-ends only ever call these four; they never see `oxc_codegen`.

**Consequence.** The whole `R3*Metadata → output_ast → text` core needs ZERO changes. The backend
switch only has to provide (A) a parser that yields a tree the front-ends can read, and (B) a lowerer
from `output_ast` to that backend's AST + printer. The emitter is the single file that has to learn a
second backend; the front-ends share a thin parse helper.

## 2. The oxc → swc API mapping

Difficulty: **trivial** = mechanical rename; **moderate** = real code, contained; **hard** = an
architectural gap that needs a deliberate adapter (called out in §3).

| Concern | OXC (today) | SWC equivalent | Difficulty | Notes / where it bites |
|---|---|---|---|---|
| Arena / lifetimes | `oxc_allocator::Allocator` passed explicitly; `Box`→`ArenaBox`, `Vec`→`ArenaVec`; `'a`-borrowed AST | `swc_common::GLOBALS` (thread-local `SourceMap`) + `swc_allocator ^4.0.1`; owned `Box`/`Vec` | **hard** | Explicit-borrow vs implicit-thread-local. Every SWC op must run inside `GLOBALS.set(&globals, ‖ … ‖)`. The backend trait owns this context so callers never see it (§3). |
| Move-into-arena | `oxc_allocator::TakeIn` (`ts_erase.rs`) | `std::mem::take` / `std::mem::replace` on owned nodes | moderate | SWC owns its nodes, so a plain `mem::take` replaces the `take_in(alloc)` dance. Only used in the JSX eraser. |
| Parse | `Parser::new(&alloc, src, SourceType).parse()` → borrowed `Program<'a>` | `swc_ecma_parser::parse_file_as_program(&fm, syntax, target, comments, &mut errs)` → owned `Program` | moderate | SWC needs a `SourceFile` from the `SourceMap` (inside GLOBALS); returns owned, no `'a`. |
| Source type / syntax | `SourceType::ts()` / `::tsx()` | `Syntax::Typescript(TsSyntax { tsx, decorators, .. })` | trivial | Direct enum swap. tsx flag maps 1:1. |
| AST node enums | `oxc_ast::ast::{Program, Expression, Statement, Pattern, Declaration, …}` (borrowed) | `swc_ecma_ast::{Program, Expr, Stmt, Pat, Decl, …}` (owned `Box`/`Vec`) | moderate | Field order differs (OXC = eval order, SWC = tsc order); our front-ends read by name, not position, so order is irrelevant. Naming differs (`Expression`→`Expr`, `Statement`→`Stmt`). |
| Span | `oxc_span::Span` (u32 start/end) + `SPAN` const | `swc_common::Span` (`BytePos(u32)` lo/hi, plus `SyntaxContext`) | trivial | Both are byte ranges. `SPAN` → `swc_common::DUMMY_SP`. The +1/`BytePos` base offset is handled once in the source-map adapter. |
| Span → source slice | `&src[span.start..span.end]` (self-contained) | needs `SourceMap` to map `BytePos`→offset | moderate | OXC spans are absolute byte offsets; SWC `BytePos` is relative to the `SourceFile` start in the map. Front-ends that re-slice source text (e.g. `linker.rs` reading a decorator argument span) go through a `span_text(node) -> &str` helper on the parse backend, not raw indexing. |
| GetSpan | `oxc_span::GetSpan::span()` | `swc_common::Spanned::span()` | trivial | Same idea, different trait name. |
| Read-only visit | `oxc_ast_visit::Visit` | `swc_ecma_visit::Visit` | trivial | Auto-generated both sides; near-identical signatures (`visit_expr(&mut self, e: &Expr)`). |
| Mutable visit | `oxc_ast_visit::VisitMut` | `swc_ecma_visit::VisitMut` | trivial→moderate | SWC nodes are owned `Box<T>`, so in-place mutation sometimes needs re-wrapping vs OXC's borrowed mutate. |
| Transform | `Traverse` (`visit_expr(&self,&Expr)->Expr`) | `Fold` (`fold_expr(&mut self, Expr)->Expr`, by value) — or newer `VisitMut + Pass` | moderate | We barely use whole-tree transforms; the JSX lowering rewrites by hand. Where we do, `Fold` takes ownership (matches SWC's owned model). |
| AST builder | `oxc_ast::AstBuilder` (`ast.program(...)`, `ast.expression_*` into arena) | direct struct literals / `Box::new(...)`, optional `swc_ecma_utils` builders | moderate | This is the **bulk of the emit port**: every `self.ast.<node>(…)` in `emitter.rs` becomes an owned `swc_ecma_ast` struct. Mechanical but large (one `Lowerer` per backend). |
| Codegen | `oxc_codegen::Codegen::default().build(&program).code` → `CodegenReturn` | `swc_ecma_codegen::Emitter { cfg, cm, comments, wr }` writing into a `Vec<u8>` | moderate | OXC is stateless and returns a string; SWC is a writer-based `Emitter`. Wrapped so both return `String`. |
| Source map | `CodegenReturn.map` (oxc sourcemap) | `Emitter` source-map output via `cm` + `SourceMapBuilder` | moderate | Both can emit v3; our `output/source_map.rs` already owns the v3 shaping (`byte_offset_to_line_col`, `utf16_columns`), so the adapter only feeds it raw mappings. |
| Parser metadata | `ParserReturn { program, errors, module_record, comments, … }` (unified) | separate: program + `errors: Vec<Error>` + a `Comments` sink you pass in | moderate | Backend trait normalizes both into one `ParseOutput { program, errors }`. |
| **Semantic** | `oxc_semantic::{Semantic, Scope, SymbolId, Reference}` | **no equivalent** | **hard** | NOT on the Ivy hot path today. Kept OXC-only by design (§3); never required of the SWC backend. |

The three genuinely hard things — **arena vs GLOBALS**, **borrowed vs owned AST**, **Visit vs Fold**
— are all *inside* the backend implementation. They do not leak to `treaty_ivy` or the front-ends if
the seam is drawn at the trait boundary in §3.

## 3. Backend abstraction design

Two complementary mechanisms: a **Cargo feature** that picks the engine at build time, and a **Rust
trait** that the parse-side front-ends program against. Emit is feature-gated rather than
trait-objected because it is a single file and the hot path benefits from monomorphization.

### 3.1 Cargo feature (the coarse switch)

A new leaf crate `libs/treaty-ivy/backend` (crate name `treaty_ivy_backend`) owns the engine
dependency and exposes the abstraction. Mutually-exclusive features, `oxc` default:

```toml
# libs/treaty-ivy/backend/Cargo.toml
[features]
default = ["oxc"]
oxc = ["dep:oxc_allocator", "dep:oxc_parser", "dep:oxc_ast", "dep:oxc_ast_visit",
        "dep:oxc_codegen", "dep:oxc_span"]
swc = ["dep:swc_common", "dep:swc_ecma_ast", "dep:swc_ecma_parser",
       "dep:swc_ecma_codegen", "dep:swc_ecma_visit", "dep:swc_allocator"]
```

`treaty_ivy_core`, `…/facade`, and `apps/rust/authoring` re-export the feature and depend on
`treaty_ivy_backend` instead of importing `oxc_*` directly. A `compile_error!` guard rejects building
with both or neither feature. The default build is bit-for-bit what ships today (only the `oxc`
feature is on, the SWC deps are not even compiled). The two crates that hold the actual engine code
(`emitter.rs`, the parse helper) become `#[cfg(feature = "oxc")]` / `#[cfg(feature = "swc")]` module
pairs behind one public name.

### 3.2 Parse trait (`ParseBackend`)

Front-ends today each call `Parser::new(...).parse()` and then pattern-match on `oxc_ast` enums. We
cannot make `oxc_ast::Expression` and `swc_ecma_ast::Expr` the *same* type, so the seam is drawn at
**"what the front-ends actually need from a parse tree"**, which is narrow:

1. parse a TS/TSX string into a tree;
2. find the class(es) with an Angular decorator and read the decorator name + argument object;
3. read an `ObjectExpression` of literal/array/identifier properties into Treaty values
   (selector, template string, `standalone`, inputs/outputs, the `ɵɵngDeclare*` declaration object);
4. recover the source text of a node's span.

That is a *metadata-extraction* surface, not a general AST surface. So the trait yields Treaty's own
neutral structs, and each backend implements the walk in its native AST:

```rust
// libs/treaty-ivy/backend/src/parse.rs
pub trait ParseBackend {
    /// Parse a module; SWC impl runs this inside GLOBALS.set(...) internally.
    fn parse_module(&self, src: &str, ty: TreatySourceType) -> ParseOutput;

    /// Source text of a node's span (oxc: direct slice; swc: via SourceMap).
    fn span_text<'s>(&self, src: &'s str, span: TreatySpan) -> &'s str;
}

/// Engine-neutral parse result the front-ends consume. NO oxc/swc types here.
pub struct ParseOutput {
    pub classes: Vec<ClassWithDecorators>,   // class name, decorators, members
    pub ng_declare_calls: Vec<NgDeclareCall>, // kind + object literal, for the linker
    pub errors: Vec<String>,
}
```

`ClassWithDecorators` / `NgDeclareCall` carry an already-lowered **object-literal model**
(`ObjLit { props: Vec<(String, LitValue)>, span }`) so `source_compile.rs` and `linker.rs` stop
matching on `oxc_ast::ObjectExpression` directly and instead read `ObjLit`. This is the one real
refactor on the parse side: move the "walk this object expression into R3 metadata" logic out of the
oxc-typed front-ends and into the OXC `ParseBackend` impl, returning neutral `ObjLit`. The SWC impl
produces the same `ObjLit` from `swc_ecma_ast::ObjectLit`.

> Note on the JSX/`.treaty` authoring front-ends (`apps/rust/authoring`): these do heavier
> *structural* AST surgery (TS erasure, control-flow rewriting) than the Ivy facade. They are
> abstracted **last** (§5, phase 4) and may keep an engine-specific module pair rather than going
> fully through `ParseOutput`, because their value-add is the structural transform, not metadata
> extraction. The Ivy facade is where backend parity matters first.

### 3.3 Emit (`EmitBackend`) — feature-gated, not trait-objected

Emit is the parity-critical path and a single file. We keep ONE public API and swap the
implementation by feature:

```rust
// libs/treaty-ivy/core/src/output/emitter.rs  (public API unchanged)
pub fn emit_statements(stmts: &[o::Stmt]) -> String { backend::emit_statements(stmts) }
pub fn emit_expression(expr: &o::Expr) -> String { backend::emit_expression(expr) }
pub fn emit_statements_with_map(...) -> (String, SourceMap) { backend::emit_statements_with_map(...) }
pub fn emit_expression_with_map(...) -> (String, SourceMap) { backend::emit_expression_with_map(...) }

#[cfg(feature = "oxc")] use crate::output::emitter_oxc as backend; // today's Lowerer, verbatim
#[cfg(feature = "swc")] use crate::output::emitter_swc as backend; // owned-AST Lowerer
```

`emitter_oxc.rs` is the existing `Lowerer` moved unchanged. `emitter_swc.rs` is the SWC port of the
same lowering, sharing `output/source_map.rs` (already engine-neutral — it works on byte offsets and
UTF-16 columns, not oxc types). Because `output_ast` (`o::Stmt`/`o::Expr`) is owned and
backend-free, both lowerers consume the identical input. Emitter behavior choices that today are
"oxc_codegen decides" (precedence-based parenthesization, see the module header in `emitter.rs`
§7.1/§7.2) must be matched on the SWC side — the parity harness (§4) is what proves they are.

### 3.4 Why this seam holds the 1:1 invariant

The invariant "same Ivy regardless of backend" is structurally guaranteed for everything
*downstream* of `output_ast`, because `R3*Metadata → output_ast` contains no engine types — only the
final lowering differs. The only place two backends can diverge is:

1. **Emit** — different printer formatting (whitespace, quote style, parenthesization, number
   literals). Caught by §4 byte-equality on emitted text.
2. **Parse → metadata** — a decorator/object literal read differently (e.g. a numeric vs string
   literal, a `forwardRef` unwrapped differently). Caught by §4 because a metadata difference changes
   the emitted text.

Both divergence classes terminate in *emitted text*, so a single byte-equality gate on the corpus is
sufficient to keep the backends 1:1.

## 4. The 1:1 parity harness (CI gate) — `tools/backend-parity`

> Status: the parity harness (§4) is now scaffolded at `tools/backend-parity` and gated in CI
> (`parity` + `drift` in `.github/workflows/rust-tests.yml`); see `migration/BACKEND-PARITY.md`.

Mirror the deterministic NO-AI shape of `tools/render3-sync` (a Rust CLI that exits non-zero on
drift; see `tools/render3-sync/src/main.rs` and its `drift` / `codegen-verify` subcommands). The
backend gate is conceptually `render3-sync drift`, but the two sides being diffed are
**oxc-built Treaty** vs **swc-built Treaty**, not Treaty vs Angular.

### 4.1 What it diffs

For every fixture in a shared corpus, compile the **same input** through both backends and assert the
emitted Ivy is **byte-identical**:

```
fixture ──┬─▶ treaty (--features oxc) ─▶ ivy_oxc.js ─┐
          └─▶ treaty (--features swc) ─▶ ivy_swc.js ─┴─▶ assert byte-equal
```

Corpus = the union of the existing fixture sources, so we reuse what already proves Angular parity
and get backend parity for free:

- The oracle fixtures in `libs/treaty-ivy/facade/parity/parity.mjs` (`FIXTURES` array:
  `static-element`, `interpolation`, …) — these already feed identical `(template, selector,
  className)` to a compiler.
- Angular's compliance corpus driven by
  `libs/treaty-ivy/facade/compliance/run-compliance.mjs`.
- Real published packages from `libs/treaty-ivy/facade/tests/link_real_packages.rs` (the partial
  linker path) — round-trip each through both backends.

### 4.2 Mechanism (no NAPI rebuild dance)

Because both backends are the *same Rust crate* under different features, the cleanest gate is a
**single test binary compiled twice**:

```
tools/backend-parity/
  Cargo.toml          # depends on treaty_ivy; re-exports its oxc/swc features
  src/main.rs         # `emit-corpus`: read corpus, compile each, write {id -> ivy text} JSON
  src/lib.rs          # corpus loader (shared with render3-sync fixture dirs)
  corpus/             # symlink/glob to the parity + compliance + real-package fixtures
```

- `cargo run -p backend-parity --no-default-features --features oxc -- emit-corpus --out oxc.json`
- `cargo run -p backend-parity --no-default-features --features swc -- emit-corpus --out swc.json`
- `cargo run -p backend-parity -- diff oxc.json swc.json` → exits 1 on any byte diff, prints a
  per-fixture `PARITY <id> OK|DIFF` table (same report style as `conformance.rs`'s PASS/DIFF lines).

Each `*.json` is a deterministic `{ fixture_id -> emitted_ivy_string }` map (BTreeMap-ordered), so
`diff` is a stable set-comparison and the artifacts can be committed as a tripwire like
`tools/render3-sync/baseline.json`.

### 4.3 Normalization policy — strict by default

The render3 oracle harness (`parity.mjs`) *normalizes* (strips whitespace, sorts args) because it is
comparing two *different compilers* (Angular vs Treaty). The backend gate is comparing **one compiler
through two printers**, so the bar is higher: default to **raw byte equality** with NO
normalization. If oxc_codegen and swc_codegen legitimately differ in some formatting that we accept
(e.g. trailing-newline, quote preference), encode that as an explicit, reviewed normalization in
`backend-parity` (a named transform with a comment justifying it) rather than silently loosening.
The goal is that the SWC emitter is *tuned* to match oxc_codegen output, and divergences are bugs to
fix, not noise to normalize away.

### 4.4 CI wiring

Add one job to the existing pipeline (next to the render3-sync drift gate):

```yaml
backend-parity:
  - cargo build -p treaty_ivy --no-default-features --features swc   # SWC backend must compile
  - cargo run  -p backend-parity --no-default-features --features oxc -- emit-corpus --out /tmp/oxc.json
  - cargo run  -p backend-parity --no-default-features --features swc -- emit-corpus --out /tmp/swc.json
  - cargo run  -p backend-parity -- diff /tmp/oxc.json /tmp/swc.json  # exit 1 on any byte diff
```

The job is green only when every fixture emits byte-identical Ivy under both backends. This is the
contract that keeps the backends 1:1, enforced mechanically on every PR. It also fails closed: if the
SWC backend stops compiling, the build step fails before the diff.

## 5. Phased rollout (abstract the seam, don't fork the compiler)

Order chosen so each phase is independently shippable, the default OXC build never regresses, and the
*narrowest* surface (emit, one file) is proven before the *widest* (JSX authoring).

**Phase 0 — extract the backend crate, OXC-only.** Create `libs/treaty-ivy/backend` with the `oxc`
feature wired and the `ParseBackend`/emit indirection in place, but only the OXC impl. Move the
`Lowerer` from `emitter.rs` into `emitter_oxc.rs` behind the public functions. Net behavior change:
zero. This proves the seam compiles and the public API is unchanged (existing tests stay green). Add
the `compile_error!` both/neither guard.

**Phase 1 — stand up `tools/backend-parity` against OXC only.** Build the corpus loader + `emit-corpus`
+ `diff`, and run it oxc-vs-oxc. It must report 100% PARITY OK trivially (identical builds). This
lands the gate infrastructure and the committed baseline format *before* there is a second backend to
diff, so the harness itself is reviewed in isolation.

**Phase 2 — SWC EMIT backend (`emitter_swc.rs`).** The single highest-value, most-contained port:
one file, owned-AST lowering of `output_ast`, sharing `output/source_map.rs`. Wire the `swc` feature.
Now `backend-parity diff` does real work: the corpus is fed through the (still OXC) parser front-end,
but emitted through each backend. Tune `emitter_swc.rs` until byte-equal across the whole corpus.
This is where arena→owned and AstBuilder→struct-literal land, contained to one module. Ship when the
gate is green.

**Phase 3 — SWC PARSE backend for the Ivy facade.** Implement `ParseBackend` for SWC and migrate
`source_compile.rs` + `linker.rs` to consume `ParseOutput`/`ObjLit` instead of `oxc_ast` directly
(the §3.2 refactor). Run the full `--features swc` build through `backend-parity` including the
`link_real_packages.rs` real-package corpus. After this phase, the **entire treaty_ivy Ivy path**
(source-decorator + partial-linker) is backend-switchable and gated 1:1.

**Phase 4 — SWC authoring front-ends (optional / last).** `apps/rust/authoring/src/jsx/*` and
`angular_source.rs` do structural transforms (TS erasure via `TakeIn`, control-flow rewriting). These
get the engine-specific-module-pair treatment (§3.2 note) rather than the metadata-extraction trait,
because their job *is* AST surgery. Only do this phase if a host actually needs SWC authoring; the
Ivy compile path (phases 0–3) is the part that matters for "plug into a host that embeds SWC."

**Anti-fork guardrails throughout:**

- OXC stays `default`; CI always builds and gates the default path first. A broken SWC backend can
  never block the OXC ship.
- No `oxc_*` import survives outside `treaty_ivy_backend` (enforce with a grep gate / `cargo deny`
  banned-import check in CI, same spirit as the `no-regex-verification` discipline).
- One IR (`output_ast`), one emitter public API, one parse-result struct (`ParseOutput`). The
  backends differ only in their *implementation* modules — never in the types `treaty_ivy` or the
  facade consume. That single-IR rule is what makes "1:1 forever" tractable instead of a perpetual
  two-compiler reconciliation.

## 6. Risks / open questions

- **SWC codegen formatting parity is the long pole.** oxc_codegen's precedence-based
  parenthesization (the documented divergence in `emitter.rs` §7.1/§7.2) must be reproduced by
  `swc_ecma_codegen` config or post-processing. Mitigation: §4.3 strict byte gate surfaces every
  divergence as a failing fixture immediately; tune the SWC `Emitter` Config (target, minify off,
  ascii/quote settings) until green.
- **GLOBALS re-entrancy / threading.** `compileMany` / rayon parallel compile (see the
  performance/parallel memory) must not share one `GLOBALS` across threads. The SWC `ParseBackend`
  and `emitter_swc` each establish their own `GLOBALS.set` scope per compile unit; verify no
  thread-local leaks under the parallel harness.
- **Binary size / build time.** Pulling `swc_*` in is heavy. Mitigated by it being off by default
  and a separate feature — default users never compile it.
- **Authoring front-end abstraction may not pay off** (phase 4). Keep it optional; the Ivy path is
  the contractually-gated part.

## 7. File map (what gets created / touched)

Created:
- `libs/treaty-ivy/backend/Cargo.toml`, `…/src/lib.rs`, `…/src/parse.rs` — the backend crate + `ParseBackend` trait + neutral `ParseOutput`/`ObjLit`.
- `libs/treaty-ivy/core/src/output/emitter_oxc.rs` — today's `Lowerer`, moved verbatim.
- `libs/treaty-ivy/core/src/output/emitter_swc.rs` — SWC lowering (phase 2).
- `tools/backend-parity/` — the parity CLI + corpus loader + committed baseline (phase 1).

Touched:
- `Cargo.toml` (workspace) — add `libs/treaty-ivy/backend` and `tools/backend-parity` members.
- `libs/treaty-ivy/core/Cargo.toml`, `libs/treaty-ivy/facade/Cargo.toml` — depend on
  `treaty_ivy_backend`, re-export `oxc`/`swc` features, drop direct `oxc_*` deps from the facade.
- `libs/treaty-ivy/core/src/output/emitter.rs` — becomes the four-function feature dispatcher.
- `libs/treaty-ivy/facade/src/source_compile.rs`, `…/src/linker.rs` — consume `ParseOutput`/`ObjLit`
  (phase 3).
- `apps/rust/authoring/src/**` — phase 4 only, if pursued.

Unchanged (the point of the design):
- `libs/treaty-ivy/core/src/output_ast.rs` — the neutral IR.
- `libs/treaty-ivy/core/src/output/source_map.rs` — engine-neutral (byte offsets / UTF-16 columns).
- The entire `R3*Metadata → output_ast` core in `treaty_ivy`.
