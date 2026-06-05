# OXC migration harness — future oxc bumps are mechanical

oxc is now **contained behind the `ParseBackend` trait**. The whole AST→Ivy
lowering walk is engine-neutral (`treaty_ivy_core::neutral`), and the only code
that names `oxc_*` AST types is `libs/treaty-ivy/facade/src/parse/oxc.rs` (the
`OxcParseBackend`) plus the feature-gated oxc emit `Lowerer`
(`core/src/output/emitter.rs`, `#[cfg(feature = "oxc")]`). A fully-native,
**zero-oxc** swc backend compiles the entire corpus byte-identically
(`--no-default-features --features swc`).

Because of that containment, a future oxc version/API change touches **only**
`parse/oxc.rs` + the oxc Cargo features — not the compiler core. So bumps are a
re-runnable harness step, not a hand-port.

## The re-runnable bump-and-gate path

1. **Scout** the oxc API surface that changed (one read-only pass over
   `parse/oxc.rs` — the only oxc-AST consumer):
   - `Workflow`/skill: `api-breakage-scout` (inventories every `oxc_*` symbol +
     its replacement → a migration crib).

2. **Bump** the oxc version in `Cargo.toml` (workspace) and migrate `parse/oxc.rs`:
   - `Workflow`/skill: `oxc-migrate` (per-module isolated-worktree rewrite →
     `cargo check` → adversarial parity review).

3. **Gate** byte-identity — the same gates CI runs (`.github/workflows/rust-tests.yml`):
   ```bash
   # default (oxc) path unchanged + byte-identical
   cargo test --workspace
   # neutral parse IR identical across backends (real Angular corpus, 1225/0)
   cargo test -p treaty_ivy --features swc parse_parity
   # cross-backend emit parity, all 29 fixtures byte-identical
   cargo run --manifest-path tools/backend-parity/Cargo.toml --features oxc,swc -- parity
   # baseline tripwire (regenerate only when the change is intended + reviewed)
   cargo run --manifest-path tools/backend-parity/Cargo.toml -- drift
   # zero-oxc swc still builds + links no oxc
   cargo build -p treaty_ivy --no-default-features --features swc
   cargo tree  -p treaty_ivy --no-default-features --features swc | grep -c oxc_   # must be 0
   ```
   - matchGolden (the 185/185 Angular golden gate) for a full check:
     `TREATY_IVY_CORPUS_DUMP=<tmp> cargo test -p treaty_ivy corpus_dump -- --ignored`
     then `node libs/treaty-ivy/facade/compliance/run-compliance.mjs --cargo-dump=<tmp>`.

4. **Regenerate the baseline** only when the emit legitimately changes
   (`cargo run --manifest-path tools/backend-parity/Cargo.toml -- baseline`),
   commit it, and confirm `drift` is clean.

The swc backend is the cross-check: if an oxc bump changes emitted bytes, the
oxc↔swc parity diff (or matchGolden) catches it immediately. CI runs the same
gates on every push (Rust tests job), so drift can't merge silently.
