# render3-sync

A deterministic (no AI) harness that keeps Treaty's Rust port of Angular's template compiler
(`libs/treaty-ivy/{core,template,decorators,facade}`) 1:1 with Angular's `packages/compiler`.
It never edits the port. It tells you what Angular changed, which Rust file owns it, and whether
the mechanical tables still match.

It has three pillars, all reading the live tree read-only:

| Command | What it checks | Exit code |
| --- | --- | --- |
| `drift` | Diffs the exports of every mapped Angular file against `baseline.json`, attributes each change to the Rust file that ports it, and lists any compiler file that is neither mapped nor out of scope. | 1 on any change or unclassified file |
| `codegen-verify` | For each `Mechanical` row, emits Rust from the Angular TS and compares it with the committed Rust (enums by value, the `R3` identifier table by name and wire string, other items line by line). | 1 on a blocking row; pending rows do not block |
| `baseline` | Records the fingerprints of every mapped file at the pinned Angular commit. | 1 if the reference is missing or incomplete |

`conformance` (a separate binary) runs the `libs/treaty-ivy/facade` JS compliance and parity
harnesses and diffs their results. It is unchanged by this README's workflow.

## Files

- `ANGULAR_REF`: the single pinned Angular commit SHA. The populate script, `baseline`, `drift`
  and `codegen-verify` all default to it. A unit test checks that `baseline.json` was recorded at it.
- `populate-angular-ref.sh`: fills `tools/angular-ref` with `packages/compiler` and
  `packages/compiler-cli/test/compliance` at the pinned commit (blobless, depth-1, sparse checkout).
- `baseline.json`: the recorded export fingerprints, one entry per mapped Angular file.
- `src/symbol_map.rs`: the maintained map. `MODULE_MAP` (one row per TS file, or per export for
  `core.ts`), `OUT_OF_SCOPE_FILES` (with a reason each), and `OUT_OF_SCOPE_SYMBOLS`.
- `src/sync.rs`: the live-tree logic (`record_baseline`, `diff_baseline`, `verify_codegen`).
- `src/drift.rs`, `src/ts2rust.rs`, `src/ts.rs`, `src/report.rs`: the snapshot differ, the mechanical
  emitter, the oxc parser wrapper and the report types.

## Workflow

### 1. Populate the reference

```sh
tools/render3-sync/populate-angular-ref.sh                 # the pinned commit in ANGULAR_REF
tools/render3-sync/populate-angular-ref.sh --sha <40-hex>  # another commit, to compare
```

The script refuses to write through a symlink or into a non-empty directory that is not a checkout
of the same remote, so a locally provided `tools/angular-ref` is never overwritten. For tests, set
`ANGULAR_REMOTE` to a local clone and `--dest` to a scratch directory.

### 2. Drift

```sh
cd tools/render3-sync
cargo run --quiet --bin render3-sync -- drift                  # live tree vs baseline.json
cargo run --quiet --bin render3-sync -- drift --repo-root <dir> --baseline <file> --ref <sha>
```

`drift` prints a `DriftReport` as JSON: changed files, the port tasks they imply (`codegen` for a
mechanical owner, `manual_port` for logic, `update_map` for an export or file the map does not cover),
and `unclassified_files`. Against the pinned commit with a matching baseline it reports `CLEAN`.

To compare an older commit against the baseline, build a scratch root that links the old
reference and the live port, then point `--repo-root` at it:

```sh
mkdir -p /tmp/old-root/tools && ln -s <old-checkout> /tmp/old-root/tools/angular-ref
ln -s /home/claude/treaty/libs /tmp/old-root/libs
render3-sync drift --repo-root /tmp/old-root --baseline tools/render3-sync/baseline.json --ref <old-sha>
```

### 3. Port

For each port task, read the `rust_file` and `symbols` it names and bring the Rust in line with the
Angular diff (`git diff` of the vendored file between the two refs is the spec). A `codegen` task
only needs the mechanical table regenerated; `manual_port` tasks are hand work.

For an `update_map` task, add a row to `MODULE_MAP` for the file (or export), or an entry to
`OUT_OF_SCOPE_FILES` / `OUT_OF_SCOPE_SYMBOLS` with a one-line reason. Every new Angular compiler
file must be classified; `drift` will not stay clean until it is.

### 4. Codegen-verify

```sh
cargo run --quiet --bin render3-sync -- codegen-verify
```

Each `Mechanical` row verifies the exports it anchors. A row is `in_sync`, `drifted` (blocking,
with the diff), `unsupported` (the emitter refused an anchored export), or `missing`.

A row can be marked `pending` in `MODULE_MAP`, with a reason, while another developer is still
porting the Rust file. Then drift is reported as `pending` and does not block. If the row later
verifies in sync, it becomes `stale_pending` and blocks until the marker is removed, so a pending
marker cannot outlive the port.

### 5. Re-baseline

Once the port and the map are up to date for a new Angular commit:

1. Put the new SHA in `ANGULAR_REF`.
2. Run `populate-angular-ref.sh` (it reads that file).
3. Run `cargo run --quiet --bin render3-sync -- baseline`. This records every mapped file at the
   new commit into `baseline.json`. It fails if the reference is missing or incomplete.
4. Run `drift` (expect `CLEAN`) and `codegen-verify` (expect no blocking rows).
5. Run `cargo test`, then commit `ANGULAR_REF`, `baseline.json` and the map changes together.

## Mapping rules

- A TS file under `packages/compiler/src` is either mapped (has `MODULE_MAP` rows) or out of scope
  (`OUT_OF_SCOPE_FILES`). Anything else is `unclassified` and `drift` reports it. A test checks the
  whole vendored tree.
- Anchors are per file. `anchor_symbols` name the exports a row owns, and only rows of the same
  TS file are consulted. An export resolves in this order (`owners_for_export`): the rows whose
  anchors name it; else the file's un-anchored rows (file-level owners); else the file's only row,
  if it has exactly one. An export that none of these claims is unmapped and drift asks for a map
  row. A file with several rows must anchor every export (or list it in `OUT_OF_SCOPE_SYMBOLS`),
  so an export never fans out to every owner of the file. Tests check that every anchor is a real
  export of its file.
- A file that is ported inside a larger Rust file names that file as its `rust_file`. The planned
  home of the pipeline phases is `template/src/view/template.rs`. `boundary.rs` is planned for
  `r3_boundaries.ts` and `r3_content_blocks.ts` and is `pending` until it exists.
- `kind: Mechanical` is set only when the emitter's output shape matches the committed item. The
  emitter does not guess: anything outside its subset is an `unsupported` entry.

## Known limits

- `spec` paths (`migration/render3-specs/*.md`) point at files that are not in this repository.
  They are carried through to tasks as pointers only.
- Only the `Mechanical` rows are verified against generated Rust. Logic rows are checked by drift
  only. The `AttributeMarker` and `SelectorFlags` constants, `InputFlags` and `InjectFlags` are
  hand-written as consts or newtypes, so their shape is not mechanical.
- The `R3` identifier check is order-insensitive, because the committed port groups names by
  purpose rather than in TS order. It compares the variant set and each variant's wire string.
- Two `core.ts` rows are `pending` and are NOT caused by the 78d2a67 bump (`core.ts` is identical
  at the old and new refs). `ViewEncapsulation` lacks `ExperimentalIsolatedShadowDom = 4` and
  `ChangeDetectionStrategy` lacks the `Eager = 1` alias, both in `decorators/src/compiler.rs`. They
  are listed by `codegen-verify` and do not block. Porting them removes the marker, and the gate
  then requires it to be removed.
- `config.ts` (`ENABLE_TEMPLATE_SOURCE_LOCATIONS`) has no Rust consumer: Treaty emits no
  source-location instructions. The row is a pointer for drift only.
- `AttributeMarker` and `SelectorFlags` are partial ports (only the markers and flag bits the Rust
  code uses). Their rows are Logic, so drift reports changes but `codegen-verify` does not check them.
- `tools/angular-ref` is untracked and is NOT ignored by `.gitignore`: the pattern `/tools/angular-ref/`
  has a trailing slash, so it matches only a directory, not the symlink. Do not `git add` it. Adding
  the line `/tools/angular-ref` (no slash) to `.gitignore` would fix it; that file is outside this tool.

## CI (suggested step, not yet added)

`.github/workflows` is not owned by this tool. This is the step to add to run the harness in CI:

```yaml
- name: render3-sync (drift + codegen-verify)
  run: |
    tools/render3-sync/populate-angular-ref.sh
    cd tools/render3-sync
    cargo test --quiet
    cargo run --quiet --bin render3-sync -- drift
    cargo run --quiet --bin render3-sync -- codegen-verify
```

`drift` exits 1 on any change and `codegen-verify` exits 1 on any blocking row. Pending rows are
printed but do not fail the step.
