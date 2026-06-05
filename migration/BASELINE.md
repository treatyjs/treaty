# Migration Baseline — `migration/v22-oxc133`

Captured at start of the Angular 22 + OXC 0.133 migration (branch `migration/v22-oxc133`).

## Toolchain present
- cargo / rustc **1.95.0**
- node **24.7.0**, bun **1.4.0**
- nx global **22.7.5** (no local install — `node_modules` not yet installed)

## Rust baseline: DOES NOT BUILD
`cargo build --workspace` → exit 101. Root cause is **not** OXC:

- The OXC `0.29.0` crates (`oxc_ast`, `oxc_syntax`, `oxc_sourcemap`, …) **compiled fine**.
- Build fails in transitive `swc_common 0.38.0`:
  `error[E0432]: unresolved import serde::__private` (removed in current serde).
- `swc_common` is pulled in only by `swc_html_ast` / `swc_css` / `swc_visit`, declared in
  [apps/rust/authoring/Cargo.toml](../apps/rust/authoring/Cargo.toml).

### Key finding: the swc deps are DEAD
`swc` appears in **zero** `.rs` files across the entire repo. The `html/` module
(`html/parser.rs`, `html/tokenizer.rs`) is hand-rolled. Therefore the `swc_*` dependencies are
unused and can be removed now — this unblocks the build and advances the plan's "replace swc"
goal (originally Phase 3) into Phase 1a.

Full log: `baseline-cargo-build.log`.

## JS baseline: not yet measured
`node_modules` absent. To be captured after `bun install` during Phase 1b. Nx projects to gate:
apps/repl, libs/typescript/{compiler,vite}, libs/treaty/edenclient, libs/authoring/node.

## Green-gate exit criteria (Phase 1c)
- `cargo build --workspace` + `cargo test` green.
- `nx run-many -t build test lint` green across all projects.
- REPL renders a `.treaty` component end-to-end.
- DI codegen (`ɵfac`/`ɵprov`) snapshot identical pre/post OXC bump.
