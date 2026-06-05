# TREATY-IVY CRATE-SPLIT PLAN (Phase 2: design + ordered execution)

> Status: PLAN ONLY. This document is the single allowed write of this workflow.
> A separate execution workflow runs the ordered steps in §5. No source/Cargo
> file is edited here.

## 0. Decision recap (locked)

Rename the single crate `render3` (package name `render3`, dir `libs/render3/`) to
`treaty_ivy`, and carve its four module subtrees into FOUR workspace crates:

```
treaty-ivy-core  <-  treaty-ivy-template  <-  treaty-ivy-decorators  <-  treaty-ivy (facade)
```

- `treaty-ivy-core` (pkg `treaty_ivy_core`): output IR + emitter/source-map, identifiers,
  factory, the binding-expression lexer/parser/converter, **and `util.rs`** (folded in —
  see §1.4), **and the i18n message-id digest primitives** (`compute_msg_id`, `fingerprint`,
  `hash32`, `mix`, `get_u32_le` — moved here to break the only DAG cycle, see §3).
- `treaty-ivy-template` (pkg `treaty_ivy_template`): `ml_parser`, `template/*`, `binder`,
  `view::{template,queries}`, `i18n` (minus the digest primitives now in core). Deps: core.
- `treaty-ivy-decorators` (pkg `treaty_ivy_decorators`): `compiler` (the
  `compile_*_from_metadata` emitter), `pipe_module_injector`, `registry`
  (`DecoratorCompiler` trait + `DecoratorRegistry`). Deps: core + template.
- `treaty-ivy` (pkg `treaty_ivy`): the facade — `compile`, `source_compile`, plus the
  top-level re-export shim that preserves the historical surface
  (`treaty_ivy::output_ast`, `treaty_ivy::output`, `treaty_ivy::factory`,
  `treaty_ivy::identifiers`, `treaty_ivy::expression(_converter)`, `treaty_ivy::util`,
  `treaty_ivy::ml_parser`, `treaty_ivy::template`, `treaty_ivy::binder`, `treaty_ivy::i18n`,
  `treaty_ivy::view::{compiler,template,queries}`, `treaty_ivy::pipe_module_injector`,
  `treaty_ivy::compile`, `treaty_ivy::source_compile`). Deps: all three lower crates.
  External consumers only ever depend on `treaty_ivy`.

The `libs/render3/` directory does NOT move (the ROADMAP-PHASE2 line
`libs/compiler/render3 (was libs/render3)` is a separate future move — out of scope,
flagged as risk R-1). The split is structural; emitted Ivy stays byte-identical.

---

## 1. The four Cargo.toml manifests

Current `libs/render3/Cargo.toml` deps (the pool to assign from):
`oxc_allocator, oxc_ast, oxc_ast_visit, oxc_parser, oxc_codegen, oxc_span, oxc_str`
(all `= "0.133.0"`, edition `2024`).

OXC usage measured per subtree (precise `use`/path scan of `libs/render3/src`):

| subtree        | oxc crates actually referenced                                  |
|----------------|-----------------------------------------------------------------|
| core (+util)   | `oxc_allocator`, `oxc_ast`, `oxc_codegen`, `oxc_span`           |
| template_mod   | `oxc_allocator`                                                 |
| decorators     | `oxc_ast`                                                       |
| facade         | `oxc_allocator`, `oxc_ast`, `oxc_codegen`, `oxc_parser`, `oxc_span` |

> NOTE on `oxc_ast_visit` + `oxc_str`: neither appears in a `use`/path scan of any
> subtree under the current names. They are carried in the manifest today but are
> either unused or pulled transitively. EXECUTION RULE: do **not** pre-prune them in
> the manifest-design step; instead, during each carve step, add exactly the crates the
> step's `cargo build` demands. If `cargo build` of a carved crate fails on a missing
> `oxc_*`, add it; if `cargo` warns "unused manifest key"/clippy flags an unused dep,
> that crate omits it. The tables below are the *starting* assignment; the green-gate in
> each step is the source of truth. (`oxc_ast_visit`/`oxc_str` most likely belong to
> core or facade if used at all; assign on demand.)

### 1.1 `treaty-ivy-core/Cargo.toml`

```toml
[package]
name = "treaty_ivy_core"
version = "0.1.0"
edition = "2024"

[dependencies]
oxc_allocator = "0.133.0"
oxc_ast       = "0.133.0"
oxc_codegen   = "0.133.0"
oxc_span      = "0.133.0"
# add on demand if a carve-step build requires them: oxc_ast_visit, oxc_str
```

### 1.2 `treaty-ivy-template/Cargo.toml`

```toml
[package]
name = "treaty_ivy_template"
version = "0.1.0"
edition = "2024"

[dependencies]
treaty_ivy_core = { path = "../treaty-ivy-core" }   # adjust relative path to chosen layout (§2)
oxc_allocator   = "0.133.0"
```

### 1.3 `treaty-ivy-decorators/Cargo.toml`

```toml
[package]
name = "treaty_ivy_decorators"
version = "0.1.0"
edition = "2024"

[dependencies]
treaty_ivy_core     = { path = "../treaty-ivy-core" }
treaty_ivy_template = { path = "../treaty-ivy-template" }
oxc_ast             = "0.133.0"
```

### 1.4 `treaty-ivy/Cargo.toml` (the facade, keeps the friendly `treaty_ivy` name)

```toml
[package]
name = "treaty_ivy"
version = "0.1.0"
edition = "2024"

[dependencies]
treaty_ivy_core       = { path = "../treaty-ivy-core" }
treaty_ivy_template   = { path = "../treaty-ivy-template" }
treaty_ivy_decorators = { path = "../treaty-ivy-decorators" }
oxc_allocator = "0.133.0"
oxc_ast       = "0.133.0"
oxc_codegen   = "0.133.0"
oxc_parser    = "0.133.0"
oxc_span      = "0.133.0"
```

**util.rs placement — DECISION: fold into `treaty-ivy-core`.** `src/util.rs` imports ONLY
`crate::output_ast` (`use crate::output_ast::{self as o, Expr, LeadingComment, Stmt, Type}`)
— no template/decorator deps — so it sits cleanly in core. It is consumed by
core/factory.rs, decorators/{compiler,pipe_module_injector,registry}.rs and
facade/{compile,source_compile}.rs; all of those depend on core anyway. A separate
`treaty-ivy-util` micro-crate would add a manifest + edge for zero benefit. It becomes
`treaty_ivy_core::util`; the facade re-exports it as `treaty_ivy::util`, and consumers'
`render3::util::{R3CompiledExpression, R3Reference}` becomes `treaty_ivy::util::{...}`.

**Dependency edges (the DAG):**
```
treaty_ivy_template   --> treaty_ivy_core
treaty_ivy_decorators --> treaty_ivy_core, treaty_ivy_template
treaty_ivy            --> treaty_ivy_core, treaty_ivy_template, treaty_ivy_decorators
```

Workspace `members` in `d:\dev\treaty\Cargo.toml` change from the single
`'libs/render3'` to the four crate dirs chosen in §2.

---

## 2. Directory layout

**Chosen layout (nested under one parent, matching the carve grouping):**

```
libs/treaty-ivy/
  core/         -> pkg treaty_ivy_core        (from libs/render3/src/core/* + util.rs + digest)
  template/     -> pkg treaty_ivy_template    (from libs/render3/src/template_mod/*)
  decorators/   -> pkg treaty_ivy_decorators  (from libs/render3/src/decorators/*)
  facade/       -> pkg treaty_ivy             (from libs/render3/src/facade/* + lib.rs shim)
```

Each of the four dirs gets its own `Cargo.toml` + `src/lib.rs`. The current subtree
module files move under each crate's `src/`:
- `core/src/`: `output_ast.rs`, `output/{mod,emitter,source_map}.rs`, `identifiers.rs`,
  `factory.rs`, `expression/{mod,ast,lexer,parser}.rs`, `expression_converter.rs`,
  `util.rs`, **`digest.rs`** (new — the i18n message-id primitives, §3).
- `template/src/`: `ml_parser.rs`, `template/{mod,r3_ast,template_transform,control_flow,deferred}.rs`,
  `binder.rs`, `view/{mod,template,queries}.rs`, `i18n.rs`.
- `decorators/src/`: `compiler.rs`, `pipe_module_injector.rs`, `registry.rs`.
- `facade/src/`: `compile.rs`, `source_compile.rs`, `lib.rs` (the re-export shim,
  ported from the current `libs/render3/src/lib.rs`).

> Relative `path =` values in §1 assume this nested layout
> (`../core`, `../template`, `../decorators` from any sibling). If the execution
> workflow instead prefers flat siblings (`libs/treaty-ivy-core`, `libs/treaty-ivy-template`,
> …), only the relative `path =` strings change — manifests + DAG are identical.
> Layout is otherwise free; nested is recommended to keep the four related crates together
> and make the future `libs/compiler/*` move (R-1) a single dir rename.

**Harness + fixtures + corpus_dump move WITH THE FACADE crate** (they exercise the public
`compile_component_source` surface, which is facade-level):
- `libs/render3/compliance/` (`run-compliance.mjs`, `COMPLIANCE-REPORT.md`) ->
  `libs/treaty-ivy/facade/compliance/`.
- `libs/render3/parity/` (`parity.mjs`, `README.md`) -> `libs/treaty-ivy/facade/parity/`.
- The `corpus_dump` test module (`#[cfg(test)] mod corpus_dump`, currently at
  `source_compile.rs:3451`, gated by env `RENDER3_CORPUS_DUMP`) and the `oracle`
  parity-shaped tests (facade `compile.rs` tests dir, the 3 `#[test]` fns around
  `compile.rs:604/628/649`) stay inside `source_compile.rs` / `compile.rs`, which already
  belong to the facade crate. No physical move beyond the facade-crate carve.

The `_cp_check` and `dbg_real` examples that appear in `target/.fingerprint/` are STALE
(no source files exist on disk) — not load-bearing, ignore. `apps/rust/authoring/examples/lex_treaty.rs`
belongs to `rust_authoring`, not render3 — untouched.

---

## 3. DAG-violation resolution (do this BEFORE the carve)

A full cross-module scan of `libs/render3/src` found **exactly one real (code) DAG cycle**:

- **CYCLE: core -> template (i18n).** `src/core/output/emitter.rs:1717` calls
  `crate::i18n::compute_msg_id(...)` inside `serialize_i18n_template_part`. `i18n` lives in
  `template_mod`, which depends on `core` — so core referencing i18n is a back-edge that
  would prevent `treaty_ivy_core` from compiling standalone.

  **FIX (concrete):** `compute_msg_id` and its private helpers (`fingerprint`, `hash32`,
  `mix`, `get_u32_le`) are PURE byte-hash primitives (ported from Angular `digest.ts`) with
  **zero** dependency on `output_ast` or any template type (verified: the only `use` lines in
  `i18n.rs` are `crate::identifiers::R3` and `crate::output_ast` — and the digest block uses
  neither). Move the digest block (currently `src/template_mod/i18n.rs` ~lines 674-820,
  the "Message-id digest — ported from digest.ts" section, plus its `#[cfg(test)]`
  digest tests) into a new `treaty_ivy_core::digest` module (`core/src/digest.rs`). Then:
  - `core/output/emitter.rs:1717` becomes `crate::digest::compute_msg_id(...)`
    (intra-crate in core).
  - `i18n.rs` re-exports for byte-compat: `pub use crate::digest::{compute_msg_id, fingerprint};`
    so any existing `crate::i18n::compute_msg_id` / external `…::i18n::compute_msg_id`
    reference still resolves (in template_mod, `crate::digest` resolves to
    `treaty_ivy_core::digest` via the core dep / a `use treaty_ivy_core as core_crate`
    alias — see §5b path-rewrite rule).
  - The facade re-exports stay: `treaty_ivy::i18n::compute_msg_id` keeps working for
    consumers (none currently call it directly, but the surface is preserved).

  Output is byte-identical: same function bodies, same call result, only the module path
  changes. This is the ONLY change required for each crate to compile standalone.

**Non-violations (doc-link only — no code dependency, but MUST be fixed to avoid broken
cross-crate intra-doc links / `-D rustdoc::broken_intra_doc_links`):**
- `decorators/compiler.rs:23,778,809`, `decorators/mod.rs:20`, `decorators/registry.rs:9,25`
  contain `[`crate::compile::…`]` / `[`crate::source_compile::…`]` intra-doc links pointing
  UP at the facade. As intra-crate links they break once facade is a separate crate.
  **FIX:** rewrite these to either a plain-backtick non-link (`` `compile::RealTemplateBuilder` ``)
  or a fully-qualified `[`treaty_ivy::compile::RealTemplateBuilder`]` link. Same for
  `template_mod/view/mod.rs:3` (`[`crate::view::compiler`]`). These are cosmetic but are
  part of each step's green gate (rustdoc warnings are not failures unless `-D` is set;
  treat as MUST-FIX to keep `cargo doc` clean).

No other back-edges exist: a scan of `template_mod/` for refs to
decorators/facade and of `decorators/` for refs to facade returned only the doc-links
above — no `use`/path code edges.

---

## 4. The `view` compatibility facade spans two crates (key carve detail)

`crate::view` is today a pure re-export facade (`src/view/mod.rs`):
```rust
pub use crate::decorators::compiler;          // -> decorators crate
pub use crate::template_mod::view::queries;   // -> template crate
pub use crate::template_mod::view::template;  // -> template crate
```
External consumer `rust_authoring` uses `render3::view::compiler::{...}` AND the template
side is reachable. After the carve, the facade crate `treaty_ivy` must REBUILD this `view`
module so `treaty_ivy::view::{compiler,template,queries}` keeps resolving:
```rust
// in treaty-ivy/facade/src/lib.rs
pub mod view {
    pub use treaty_ivy_decorators::compiler;
    pub use treaty_ivy_template::view::queries;
    pub use treaty_ivy_template::view::template;
}
```
This is the single trickiest re-export and is called out as its own sub-step in §5e.

---

## 5. Ordered, mechanical execution plan

Each step ends cargo-green and preserves the compile invariants. The technique throughout:
**rename/move, then restore the historical path via `pub use` / `extern crate` aliasing**, so
call sites change in lockstep with the boundary they cross and never in bulk.

Invariants asserted at EVERY green gate (the floor — must never regress):
- `cargo test -p <crate>` green for every crate that exists at that step.
- `cargo test -p rust_authoring` green (heaviest consumer).
- compliance `matchGolden >= 140`, `compile >= 584` (via `--cargo-dump`, see §6).
- oracle `parity.mjs` 27 PASS (non-i18n).
- byte-identical corpus dump (the `RENDER3_CORPUS_DUMP` JSON map is unchanged vs the
  pre-split baseline captured in step (a)).

> CONCURRENCY: workflow `wkqebg6l4` is editing render3 INTERNALS (factory inject codegen +
> Injectable plugin). It does NOT touch crate name, module DAG, public surface, consumer
> imports, or harness refs. Execution of this plan must REBASE onto its merged result first;
> the structural steps below remain valid. If a merge conflict touches `factory.rs` or the
> registry, take their body and re-apply only the path/manifest mechanics.

### Step 0 — baseline capture (no code change)
- Run the full gate against current `render3` and SAVE: the `RENDER3_CORPUS_DUMP` JSON,
  the matchGolden/compile numbers, the oracle count.
- Commands:
  - `cargo test -p render3` (record pass count), `cargo test -p rust_authoring`.
  - `RENDER3_CORPUS_DUMP=<tmp>\baseline-dump.json cargo test -p render3 corpus_dump -- --ignored --nocapture`
  - `node libs/render3/compliance/run-compliance.mjs --cargo-dump=<tmp>\baseline-dump.json --report`
  - `node libs/render3/parity/parity.mjs`
- Gate: numbers recorded; this JSON is the byte-identity oracle for all later steps.

### Step (a) — rename package `render3` -> `treaty_ivy` IN PLACE (one crate, no carve yet)
- Files touched:
  - `libs/render3/Cargo.toml`: `name = "render3"` -> `name = "treaty_ivy"`.
  - `Cargo.toml` (workspace): member path unchanged (`'libs/render3'`) — only the package
    name changed, dir stays.
  - Consumer manifests: `apps/rust/authoring/Cargo.toml:8`,
    `apps/treaty-cli/Cargo.toml:20`, `libs/authoring/node/Cargo.toml:12`,
    `libs/packagr/Cargo.toml:13`: rename the dep key
    `render3 = { path = ... }` -> `treaty_ivy = { path = "<same path>" }`.
  - Consumer source: rewrite every `render3::` -> `treaty_ivy::` (and doc-comment refs).
    Exact files (from inventory + scan):
    - `apps/rust/authoring/src/sfc.rs` (lines 33-46 use-block, plus 679/701/703/711 +
      doc lines 14/565/617/621), `apps/rust/authoring/src/angular_source.rs`
      (12/39/131/215).
    - `apps/treaty-cli/src/compile.rs` (6/43), `apps/treaty-cli/src/core.rs` (50).
    - `libs/authoring/node/src/lib.rs` (4/5), `libs/authoring/node/index.d.ts`
      (doc/comment refs only — verify, the `.d.ts` is generated; if so, regenerate).
    - `libs/packagr/src/compile.rs` (6/34).
  - Harness env-var + invocation refs (these are the ONLY load-bearing harness refs;
    directory paths `libs/render3/...` stay valid since the dir does not move yet):
    - `libs/render3/compliance/run-compliance.mjs`: the documented test invocation
      `cargo test -p render3 corpus_dump` -> `cargo test -p treaty_ivy corpus_dump`,
      and the env var `RENDER3_CORPUS_DUMP` -> `TREATY_IVY_CORPUS_DUMP` (rename in the
      `.mjs` comment AND in `source_compile.rs`'s `std::env::var("RENDER3_CORPUS_DUMP")`
      at ~line 3514 + the `RENDER3_CORPUS_DUMP=...` doc lines 3448). Keep them in sync.
    - `.claude/settings.json` / `.claude/settings.local.json`: the many
      `cargo test -p render3 …` allowlist entries -> `treaty_ivy`. (Permissions only;
      not a build gate, but update so the harness keeps auto-approving.)
  - `lib.rs` top doc comment: `//! `render3` —` -> `//! `treaty_ivy` —` (cosmetic).
  - DO NOT touch: `tools/render3-sync/*` (its `"render3/r3_*.ts"` strings are Angular's
    UPSTREAM `packages/compiler/src/render3/*` paths, NOT this crate), `migration/render3-specs/*`,
    `PORT-ARCHITECTURE.md`, `ROADMAP-PHASE2.md`. (Disambiguation per inventory.)
- Prove green: `cargo test -p treaty_ivy`, `cargo test -p rust_authoring`,
  `cargo test -p treaty_cli`, `cargo build -p authoring_node`, `cargo build -p treaty_packagr`;
  then `TREATY_IVY_CORPUS_DUMP=<tmp>\a-dump.json cargo test -p treaty_ivy corpus_dump -- --ignored --nocapture`
  and `node libs/render3/compliance/run-compliance.mjs --cargo-dump=<tmp>\a-dump.json --report`.
- Invariant: `a-dump.json` BYTE-IDENTICAL to `baseline-dump.json`; matchGolden>=140,
  compile>=584; oracle 27. (Pure rename ⇒ identical emit.)

### Step (b) — carve `treaty-ivy-core` (+ util + digest)
- Pre-req: apply the §3 digest move (create `core/src/digest.rs`, rewrite
  `emitter.rs:1717` to `crate::digest::…`, add `i18n.rs` re-export) — this can be done
  in place in the still-single crate first and proven green, OR atomically with the carve.
  Recommend: do the digest move as its own micro-commit (still single crate), green, THEN carve.
- Create `libs/treaty-ivy/core/` with `Cargo.toml` (§1.1) + `src/lib.rs`; move the core
  subtree files (§2) into it. `core/src/lib.rs` declares `pub mod output_ast; pub mod output;
  pub mod identifiers; pub mod factory; pub mod expression; pub mod expression_converter;
  pub mod util; pub mod digest;`.
- Add `treaty_ivy_core = { path = ... }` to the (still-named) `treaty_ivy` facade-crate
  manifest. In the remaining crate, rewrite core-boundary paths: every `crate::output_ast`,
  `crate::output*`, `crate::identifiers`, `crate::factory`, `crate::expression*`,
  `crate::expression_converter`, `crate::util` reference that now lives in template/decorators/
  facade code becomes `treaty_ivy_core::…`. MECHANICAL RULE: add at the top of each remaining
  crate `use treaty_ivy_core as core;` is NOT enough (the historical aliases are top-level);
  instead keep the lib.rs `pub use treaty_ivy_core::{output_ast, output, identifiers, factory,
  expression, expression_converter, util};` so `crate::output_ast` etc. still resolve inside
  the not-yet-carved remainder. This makes the per-file body edits ZERO until that subtree is
  itself carved.
- Add `libs/treaty-ivy/core` to workspace `members`.
- Prove green: `cargo test -p treaty_ivy_core` (core's own tests, incl. moved digest tests),
  `cargo test -p treaty_ivy`, `-p rust_authoring`; corpus dump + compliance + oracle.
- Invariant: dump byte-identical; 140/584; oracle 27.

### Step (c) — carve `treaty-ivy-template`
- Create `libs/treaty-ivy/template/` (§1.2 manifest, dep core). Move `template_mod/*` files;
  `template/src/lib.rs` declares `pub mod ml_parser; pub mod template; pub mod binder;
  pub mod view; pub mod i18n;` and `pub use treaty_ivy_core::{…historical core aliases…}` so
  in-crate `crate::output_ast`-style refs in template code resolve to core.
- In template code, rewrite any `crate::<core-alias>` to resolve via the lib.rs re-use (above)
  — no per-line edit needed if the re-use shim is present. The digest re-export in `i18n.rs`
  becomes `pub use treaty_ivy_core::digest::{compute_msg_id, fingerprint};`.
- In the remaining (decorators+facade) crate, point template aliases at the new crate:
  lib.rs `pub use treaty_ivy_template::{binder, i18n, ml_parser, template};` and keep
  `pub use treaty_ivy_template::view::{template as …, queries};` wiring (the `view` facade is
  rebuilt fully in step e).
- Add to workspace `members`.
- Prove green: `cargo test -p treaty_ivy_template`, `-p treaty_ivy`, `-p rust_authoring`;
  dump + compliance + oracle. Invariant unchanged.

### Step (d) — carve `treaty-ivy-decorators`
- Create `libs/treaty-ivy/decorators/` (§1.3 manifest, deps core + template). Move
  `decorators/{compiler,pipe_module_injector,registry}.rs`; `decorators/src/lib.rs` declares
  `pub mod compiler; pub mod pipe_module_injector; pub mod registry;` + the core/template
  re-use shims so in-crate `crate::output_ast` / `crate::template::…` / `crate::view::template`
  resolve. Apply the §3 doc-link fixes here (the `[`crate::compile…`]` links -> plain or
  `[`treaty_ivy::compile…`]`).
- In the remaining (facade) crate, point decorator aliases at the new crate.
- Add to workspace `members`.
- Prove green: `cargo test -p treaty_ivy_decorators`, `-p treaty_ivy`, `-p rust_authoring`;
  dump + compliance + oracle. Invariant unchanged.

### Step (e) — facade becomes the thin top crate `treaty_ivy`
- The remaining crate (now only `compile.rs`, `source_compile.rs`, `lib.rs`) moves to
  `libs/treaty-ivy/facade/` with manifest §1.4 (deps all three lower crates). Update workspace
  `members`: REMOVE the old `'libs/render3'` entry, ADD `libs/treaty-ivy/facade` (and the three
  lower-crate dirs added in b/c/d). Delete the now-empty `libs/render3/` dir (or leave a
  tombstone README pointing to the new layout — recommend delete since no dir-path refs remain
  load-bearing after harness moved).
- `facade/src/lib.rs` is the full re-export shim (ported from current `lib.rs`):
  ```rust
  pub use treaty_ivy_core::{output_ast, output, identifiers, factory,
      expression, expression_converter, util};
  pub mod digest { pub use treaty_ivy_core::digest::*; }      // if any consumer wants it
  pub use treaty_ivy_template::{ml_parser, template, binder, i18n};
  pub use treaty_ivy_decorators::pipe_module_injector;
  pub mod view {                                              // §4 — the two-crate facade
      pub use treaty_ivy_decorators::compiler;
      pub use treaty_ivy_template::view::{queries, template};
  }
  pub mod compile;          // facade's own
  pub mod source_compile;   // facade's own (carries corpus_dump test)
  pub use self::{compile as compile_mod_alias /* keep names */};
  ```
  (The `compile`/`source_compile` modules stay physically in the facade crate; their
  historical top-level path `treaty_ivy::compile` / `treaty_ivy::source_compile` is the module
  itself, no extra `pub use` needed.)
- Move harness dirs: `libs/render3/compliance/` -> `libs/treaty-ivy/facade/compliance/`,
  `libs/render3/parity/` -> `libs/treaty-ivy/facade/parity/`. Update the relative repo-root
  probes inside `run-compliance.mjs` / `parity.mjs` (they compute `repoRoot` from
  `__dirname`; the new depth `libs/treaty-ivy/facade/{compliance,parity}` changes the number
  of `..` segments — verify and fix). Update any `libs/render3/compliance/run-compliance.mjs`
  path strings in docs/permissions to the new path.
- Consumer manifests already point at `treaty_ivy` by NAME (step a) — only the `path =`
  value changes from `libs/render3` to `libs/treaty-ivy/facade`. Update the 4 consumer
  manifests' path strings. Consumer SOURCE is unchanged (still `treaty_ivy::…`, all
  facade-level).
- Prove green: `cargo test -p treaty_ivy`, `-p treaty_ivy_core`, `-p treaty_ivy_template`,
  `-p treaty_ivy_decorators`, `-p rust_authoring`, `-p treaty_cli`,
  `cargo build -p authoring_node -p treaty_packagr`; then corpus dump + compliance + oracle
  from the NEW harness paths. Invariant: dump byte-identical to baseline; 140/584; oracle 27.

### Step (f) — full-workspace verification gate (§6)
- `cargo build --workspace` + `cargo test --workspace` green.
- Re-run the full §6 gate list end-to-end; diff `f-dump.json` against `baseline-dump.json`
  (must be byte-identical). Commit on green.

---

## 6. Verification gate list (the floor every step must hold)

1. `cargo test -p treaty_ivy` — GREEN (facade unit/integration incl. oracle + corpus_dump-defn).
2. `cargo test -p treaty_ivy_core` / `-p treaty_ivy_template` / `-p treaty_ivy_decorators` —
   GREEN (each lower crate's own ported tests; together they sum to the original `render3`
   test count — no test lost, none added by the split).
3. `cargo test -p rust_authoring` — GREEN (heaviest consumer; reaches all four layers via
   the facade surface).
4. `cargo build -p authoring_node -p treaty_packagr` + `cargo test -p treaty_cli` — GREEN.
5. Corpus dump byte-identity:
   `TREATY_IVY_CORPUS_DUMP=<tmp>\dump.json cargo test -p treaty_ivy corpus_dump -- --ignored --nocapture`
   then `diff` vs `baseline-dump.json` — ZERO bytes differ.
6. Compliance via `--cargo-dump`:
   `node <harness>/run-compliance.mjs --cargo-dump=<tmp>\dump.json --report` —
   `matchGolden >= 140` AND `compile >= 584`.
7. Oracle: `node <harness>/parity.mjs` — 27 PASS (non-i18n).
8. `cargo doc -p treaty_ivy --no-deps` — no broken intra-doc links (the §3 doc-link fixes).

`<harness>` = `libs/render3/{compliance,parity}` for steps a–d, then
`libs/treaty-ivy/facade/{compliance,parity}` from step e onward.

---

## 7. Risks / unresolved

- **R-1 (flagged, not blocking):** ROADMAP-PHASE2.md:139 anticipates moving `libs/render3` ->
  `libs/compiler/render3`. This plan does NOT do that move; it relocates to `libs/treaty-ivy/*`
  instead. If the execution workflow is later told to honor ROADMAP-PHASE2, the chosen
  `libs/treaty-ivy/*` layout makes it a single parent-dir rename. No conflict, but note the
  divergence from that doc.
- **R-2:** `oxc_ast_visit` / `oxc_str` assignment is by-demand (see §1 note); a carve step may
  surface a usage the path-scan missed (e.g. behind a macro). Resolution is mechanical: add the
  dep the failing `cargo build` names. Not a design blocker.
- **R-3:** Harness `repoRoot`/`__dirname` depth changes when compliance/parity move in step (e).
  Must re-verify the `..` segment count; a missed segment yields "fixtures not found", caught
  immediately by the step-e gate.
- **R-4 (concurrency):** workflow `wkqebg6l4`'s in-flight factory/Injectable edits must be
  merged FIRST; rebase this plan's mechanics onto the merged bodies (it touches no structural
  surface, so all steps remain valid).
- No unresolved DAG blocker remains: the single core<-template i18n cycle is fully resolved by
  the §3 digest move, verified pure (no output_ast/template deps).
