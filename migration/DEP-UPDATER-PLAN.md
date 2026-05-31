# O — self-updating dependencies + breaking-change fixer for THIS repo (no AI)

Goal: keep the treaty repo's OWN dependencies current (Rust crates — esp. oxc — and npm) and fix the
breaking changes AUTOMATICALLY and DETERMINISTICALLY (no GPU/AI/Claude), long-running. A self-hosted
renovate/dependabot + codemod-on-break, tailored to this repo. Sibling to [[ngx-maintenance]] (that one
is for external Angular libs; THIS one is for our repo).

## Pipeline (deterministic, no AI)
1. **Detect**: read the workspace manifests (root `Cargo.toml` + member crates, `package.json` +
   libs/treaty/*); query latest versions (crates.io index, npm registry). Produce an update list
   (crate/pkg → current → latest, semver class).
2. **Apply one update at a time** on a branch: bump the version (`cargo update -p x --precise` /
   edit Cargo.toml; npm/bun dep bump).
3. **Build + test**: `cargo build/test --workspace`, the oracle + compliance harness, and tsgo+oxlint
   for TS. Green ⇒ keep; open/stack a PR.
4. **On breakage ⇒ codemods (the key no-AI part)**: a registry of DETERMINISTIC codemods keyed by
   (dependency, from→to). Seed it from the existing `migration/OXC-MIGRATION-CRIB.md` (the documented
   oxc 0.29→0.133 API changes: VisitMut moved to oxc_ast_visit, AstBuilder renames, Argument
   flattening, SymbolTable+ScopeTree→Scoping, etc.) — codify each as an oxc/ast-grep/regex codemod that
   rewrites the Rust call sites. Run the matching codemods, rebuild; if green, record the codemod as
   resolving that bump. If still broken after codemods, STOP and emit a precise failing report (a human
   reviews — never an LLM).
5. **PR**: per dependency (or grouped), with the changelog + which codemods ran. Long-running via CI cron.

## Codemod engine
- Rust call-site rewrites: prefer **ast-grep** (oxc-adjacent, rule-based) or an oxc-based rewriter; the
  OXC-MIGRATION-CRIB entries become declarative rules (pattern → replacement). Extensible: each future
  oxc bump that breaks adds a rule.
- TS codemods: oxc/ts-morph for the JS packages.
- Codemods are PURE + idempotent + unit-tested against fixtures of the old→new API.

## Workflow / fan-out (new dir `tools/dep-updater/`, disjoint from libs/render3 src)
- P1 Scaffold: `tools/dep-updater/` (Rust-first crate; reads manifests, queries registries). Update-plan
  model.
- P2 (parallel): (a) detector (manifest parse + registry version query + update plan); (b) codemod
  engine + the seeded OXC-CRIB rule set (with fixture tests proving each rule rewrites old→new oxc API);
  (c) the apply/build/test/PR orchestration (scripts the bump→verify→codemod→PR loop).
- P3 Verify: unit-test the detector (parses our Cargo.toml/package.json → correct update list) + the
  codemod rules (each rewrites its old→new fixture) + a dry-run of the orchestration (no real PR).

## Constraints
NO AI/Claude/GPU. Rust-first. tsgo/oxlint for TS. Deterministic + idempotent; long-running CI cron.
Does NOT edit libs/render3 src while compliance runs (it's a tool; applying updates is a gated run).
Reuses the repo's own gates (cargo test --workspace, oracle, compliance) as the green signal.
