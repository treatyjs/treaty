# N — render3 ↔ oxc sync harness (keep the Rust port 1:1 with Angular, no AI)

Goal: as Angular's `render3` (and any new Angular compiler) evolves, keep our Rust/oxc port **1:1**
WITHOUT AI/Claude. Honest constraint: arbitrary **TS→Rust transpilation is NOT mechanically solvable**
in general (ownership/lifetimes/control-flow). So this harness maximizes what IS deterministic and
pinpoints the rest, gated by conformance.

## Three deterministic pillars (no AI)
1. **Drift detection (the core).** Pin Angular's `packages/compiler` at a known ref (already vendored at
   `tools/angular-ref/`). On an Angular bump: `git diff <old>..<new> -- packages/compiler/src/render3
   /output /expression_parser` → a STRUCTURED report (changed files → changed exported functions/enums/
   identifier tables). Map each changed TS symbol to the Rust module that ports it (a maintained
   symbol→module map, e.g. `r3_identifiers.ts`→`identifiers.rs`, `view/template.ts`→`view/template.rs`).
   Output = a precise "what diverged + which Rust file to touch" task list.
2. **Conformance gate.** Regenerate the compliance goldens from the new Angular + run our compliance
   suite (`libs/render3/compliance`) + the oracle (`libs/render3/parity`). The set of newly-failing cases
   IS the deterministic "we are no longer 1:1 here" signal — it bounds exactly what re-porting is needed.
3. **Mechanical codegen (oxc).** For the parts that ARE mechanically portable, parse the Angular TS with
   **oxc** and EMIT Rust, diffing against our current Rust:
   - `r3_identifiers.ts` (the `ɵɵ*` instruction name table) → Rust `identifiers.rs` consts.
   - enums/flag constants: `AttributeMarker`, `SelectorFlags`, `ChangeDetectionStrategy`,
     `ViewEncapsulation`, `RenderFlags`, `BindingFlags`, etc. → Rust enums with the same numeric values.
   - small pure lookup tables / opcode maps.
   The codegen WRITES to a staging area + diffs against `libs/render3/src`; an auto-PR updates the
   mechanical tables when they only changed values/names (the safe, high-churn part). Complex logic is
   NEVER auto-transpiled — it goes to the task list (pillar 1) for deterministic hand-porting.

## "TS→Rust optimized" codegen scope (oxc)
A reusable `ts2rust` codegen (oxc parse → small IR → Rust emit) limited to the MECHANICAL subset:
const/enum/numeric tables, string-literal name maps, simple pure functions over those (the same subset
the existing `ts_to_rust.rs` server-fn transpiler covers, extended for data tables). Optimize for the
emitted Rust to be allocation-free constants. It explicitly REFUSES (reports) anything outside the
subset rather than guessing — determinism over coverage.

## Workflow / fan-out (new dir `tools/render3-sync/`, disjoint from libs/render3 src)
- P1 Scaffold: `tools/render3-sync/` (a Rust crate or TS tool — Rust preferred per Rust-first, using
  oxc to parse the vendored Angular TS). Symbol→module map config.
- P2 (parallel): (a) drift-differ (git diff Angular render3 → structured report); (b) conformance
  regen+run wrapper (goldens + compliance + oracle → failing-case report); (c) ts2rust mechanical
  codegen (identifiers + enums → staged Rust + diff).
- P3 Verify: run against the CURRENT pinned Angular (expect zero drift / mechanical codegen matches our
  committed `identifiers.rs`+enums byte-for-byte) — proves the harness is correct on a known-good state.
- Does NOT edit `libs/render3/src` while compliance runs; writes reports + staged codegen. Applying
  codegen to render3 is a separate gated step.

## Constraints
NO AI/Claude. Rust-first (the harness + codegen are Rust/oxc). tsgo/oxlint for any TS. The harness is
LONG-RUNNING-friendly (CI cron on Angular releases) and produces deterministic artifacts a human (or a
future deterministic step) acts on.
