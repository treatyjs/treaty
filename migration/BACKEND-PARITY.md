# Backend parity harness — `tools/backend-parity`

Status: scaffolded at `tools/backend-parity` (oxc baseline wired; swc cross-backend diff lands with
the swc emit backend — see `migration/SWC-BACKEND-PLAN.md` phases 1→2).
Related: `migration/SWC-BACKEND-PLAN.md` (esp. §3.3, §4), `tools/render3-sync/`,
`libs/treaty-ivy/core/src/output/emitter.rs`.

## What it is

`tools/backend-parity` is the deterministic, **NO-AI** CI gate that keeps Treaty's compile backends
**1:1**. It is modelled on `tools/render3-sync` (a plain Rust CLI that exits non-zero on drift and
commits a baseline artifact as a tripwire), but the two sides it diffs are **the same compiler
through different parser/codegen backends**, not Treaty vs Angular.

Like `tools/dep-updater` and `tools/render3-sync`, it is its **own standalone workspace** (it carries
its own `[workspace]` table), so it sits outside the parent `--workspace` build and is gated
explicitly in CI (see `.github/workflows/rust-tests.yml`). It depends on `treaty_ivy` and re-exports
its `oxc` / `swc` features, so the corpus is compiled by the *real* compiler under each backend
rather than a reimplementation.

## The 1:1 invariant it enforces

The backend choice (OXC default, SWC for hosts that already embed SWC) is a **performance /
host-integration** decision and must **never** change the compiled output. The harness mechanically
enforces that contract:

- **oxc ↔ swc byte-identical Ivy.** For every fixture in the shared corpus, the *same input* is
  compiled through each wired backend and the emitted Ivy text is asserted **byte-equal** — strict
  raw byte equality by default, no normalization. (Contrast the render3 oracle harness, which
  normalizes because it compares two *different* compilers; here it is one compiler through two
  printers, so the bar is higher.) Any accepted formatting divergence between `oxc_codegen` and
  `swc_ecma_codegen` must be encoded as an explicit, reviewed, named normalization with a justifying
  comment — never a silent loosening. See `SWC-BACKEND-PLAN.md` §4.3.
- **Both divergence classes terminate in emitted text.** A backend can only diverge in (1) emit
  (printer formatting) or (2) parse→metadata (a decorator/object literal read differently). Both
  surface as a change in the emitted Ivy string, so a single byte-equality gate over the corpus is
  sufficient to keep the backends 1:1 (`SWC-BACKEND-PLAN.md` §3.4).
- **Angular/Treaty → React emit is matched here too.** Per `SWC-BACKEND-PLAN.md` §3.3, the emit path
  is the parity-critical chokepoint, and the React emit work (the Angular/Treaty → React lowering)
  rides the same `output_ast → backend printer` seam. Its fixtures are part of this corpus, so React
  output is held byte-identical across backends by the same gate rather than by a separate harness.

The structural reason this holds: everything downstream of `output_ast` (the neutral owned IR) carries
**no engine types** — `R3*Metadata → output_ast` is backend-free, only the final lowering differs.
That single-IR rule is what makes "1:1 forever" tractable.

## The corpus

The corpus is the **union of the existing fixture sources**, so proving Angular parity already buys
backend parity:

- the oracle fixtures in `libs/treaty-ivy/facade/parity/parity.mjs` (`FIXTURES`);
- Angular's compliance corpus driven by `libs/treaty-ivy/facade/compliance/run-compliance.mjs`;
- the real published packages exercised by `libs/treaty-ivy/facade/tests/link_real_packages.rs` (the
  partial-linker path), round-tripped through each backend.

## Subcommands

The CLI exits non-zero on any failure so CI fails closed.

- **`parity`** — the primary gate. Compiles the whole corpus through every wired backend and asserts
  the emitted Ivy is byte-identical, printing a per-fixture `PARITY <id> OK|DIFF` table (same report
  style as `render3-sync`'s conformance PASS/DIFF lines). Today, with only the `oxc` feature wired,
  this is the **oxc baseline** running oxc-vs-oxc (trivially 100% OK — it proves the harness and the
  baseline format are sound *before* there is a second backend). Once the `swc` feature lands
  (`SWC-BACKEND-PLAN.md` phase 2) the same command does real **oxc ↔ swc** cross-backend work. It
  also fails closed: if the SWC backend stops compiling, the build step fails before the diff.
- **`drift`** — re-emits the live corpus and diffs the result against the **committed parity
  baseline** (a deterministic `{ fixture_id → emitted_ivy_string }` BTreeMap-ordered JSON artifact,
  the tripwire equivalent of `tools/render3-sync/baseline.json`). Exits non-zero when any fixture's
  emitted Ivy has moved out from under the baseline, so unreviewed output changes fail the build.
- **`emit-corpus --out <file>`** — the lower-level primitive the above are built on: compile each
  fixture with the currently-built backend and write the `{ id → ivy text }` JSON. Used to
  (re)record a baseline and to materialise each side of a manual `diff` when running the two feature
  builds separately, e.g.:

  ```sh
  cargo run -p backend-parity --no-default-features --features oxc -- emit-corpus --out /tmp/oxc.json
  cargo run -p backend-parity --no-default-features --features swc -- emit-corpus --out /tmp/swc.json
  cargo run -p backend-parity -- diff /tmp/oxc.json /tmp/swc.json   # exit 1 on any byte diff
  ```

## CI wiring

`.github/workflows/rust-tests.yml` gates the harness next to the other standalone-workspace crates
(`file-routing`, `dep-updater`):

```yaml
- name: backend-parity — oxc baseline + cross-backend parity (standalone workspace)
  run: cargo run --manifest-path tools/backend-parity/Cargo.toml -- parity

- name: backend-parity — drift vs committed baseline (standalone workspace)
  run: cargo run --manifest-path tools/backend-parity/Cargo.toml -- drift
```

The job is green only when every fixture emits byte-identical Ivy under every wired backend **and**
nothing has drifted from the committed baseline. OXC stays the default and is gated first; a broken
SWC backend can never block the OXC ship.

## The drift → auto-migrate loop (`.claude/workflows/backend-parity`)

CI is the *gate*; the `.claude/workflows/backend-parity` loop is the *remediation*, mirroring the
existing render3 workflows (`render3-port`, `oxc-migrate`, `ivy-parity-verify`). It is the only
AI-in-the-loop part — the harness itself stays deterministic. The loop:

1. **Detect.** Run `backend-parity parity` / `drift`. The non-zero exit plus the per-fixture
   `PARITY <id> DIFF` table is the precise, bounded "we are no longer 1:1 here" signal — it names
   exactly which fixtures diverged and shows the oxc-vs-swc byte diff.
2. **Localise.** Because every divergence terminates in emitted text and the IR is shared, a DIFF
   points at one of two contained surfaces: the emit lowering (`emitter_swc.rs` vs `emitter_oxc.rs`)
   or the parse→metadata read (`ParseBackend` impls feeding `source_compile.rs` / `linker.rs`).
3. **Auto-migrate.** Fan out a subagent to tune the lagging backend (almost always the SWC emitter's
   codegen config — parenthesization, quote style, number/whitespace formatting per
   `SWC-BACKEND-PLAN.md` §6) until the divergent fixtures go byte-equal, *without* touching the OXC
   reference output. Accepted, justified formatting differences become a named normalization in
   `backend-parity` rather than a silent loosening.
4. **Re-gate & commit.** Re-run `parity` / `drift` to green, re-record the baseline only on an
   intentional, reviewed output change, and commit on green (per the commit-cadence discipline).

The loop never edits the OXC path to make a diff disappear, and no `oxc_*`/`swc_*` import is allowed
to leak outside the backend crate — the same anti-fork guardrails as `SWC-BACKEND-PLAN.md` §5.
