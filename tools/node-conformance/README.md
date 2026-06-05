# node-conformance

A deterministic, **no-AI** Node.js compatibility conformance harness for the Treaty runtime
(`treaty_runtime`). It runs Node `test/parallel`-style `.js` test files through
`JsRuntime::with_node_compat`, classifies each as **PASS** / **FAIL** / **SKIP** (unsupported, with a
reason), and reports an aggregate pass-rate — the same kind of Node-compat scoreboard Bun and Deno
publish, but reproducible and heuristic-free: a test either runs clean (Pass), throws (Fail), or is
explicitly marked unsupported (Skip).

The harness exercises only the Node surface the runtime currently implements: `require` plus the
`node:` builtins `fs`, `fs/promises`, `path`, `process`, `buffer`, `os`, `util`, `events`, `console`,
`timers`, `url`, text encoding, and `fetch`, all on top of the runtime's event loop. Tests that
target still-unimplemented APIs (crypto, http, net, worker_threads, ...) stay tracked as **known
gaps** rather than being silently omitted — see the manifest below.

## Layout

```
tools/node-conformance/
  corpus/                  the .js conformance tests (one case per file)
  known-unsupported.json   the known-unsupported manifest (name/glob -> reason)
  src/lib.rs               report types (CaseResult, ConformanceReport) + re-exports
  src/runner.rs            the executor (run a source / walk a corpus, classify)
  src/manifest.rs          the glob manifest loader + the reporter (table + JSON)
```

## Running the harness

From the workspace root:

```sh
# Run the bundled seed corpus.
cargo run -p node_conformance

# Run a different directory of .js tests.
cargo run -p node_conformance -- path/to/tests
```

The binary prints a per-case line and a one-line summary to **stderr**, and writes the full
`ConformanceReport` as JSON to **stdout** (so CI can capture / diff it):

```sh
cargo run -p node_conformance > report.json
```

Exit code is `0` when there are no failures (skips are fine) and `1` when any case fails.

## Reading the pass-rate

`render_table` (in `src/manifest.rs`) renders a per-module + overall table. Modules are derived from
each case name's prefix before the first `-` (`fs-roundtrip` -> `fs`, `path-basic` -> `path`):

```
MODULE    PASS   FAIL   SKIP     RATE
------------------------------------------
buffer       1      0      0   100.0%
crypto       0      0      1     0.0%
fs           2      1      0    66.7%
path         1      0      0   100.0%
------------------------------------------
TOTAL        4      1      2    80.0%
```

- **RATE** is the fraction of *executed* (non-skipped) cases that passed: `passed / (passed +
  failed)`. **Skips are excluded from the denominator** — a skip is a known gap, not a regression —
  which is how Bun/Deno headline their Node-compat percentage against the tests they actually
  attempt. A module (or a whole run) that executed nothing reports `0.0%`.
- The machine-readable artifact for a scoreboard / CI diff is the `ConformanceReport` JSON, written
  with `write_report_json` (or emitted to stdout by the binary). It round-trips back through `serde`.

## Adding a Node test

1. Drop a `.js` file in `corpus/`. The **file stem is the case name**, so follow the
   `<module>-<topic>` convention (e.g. `fs-roundtrip.js`, `path-basic.js`) so the test buckets under
   the right module in the table.
2. Write it as a self-contained script that **throws on failure** — `throw new Error(...)` or a small
   inline `assert(cond, msg)` helper. Returning normally is a pass; any thrown value (including from a
   `setTimeout`/promise callback, which the runtime's event loop settles before `eval` returns) is a
   fail.
3. Use only `require("node:<builtin>")` for the implemented builtins listed above. If the test needs
   an API the runtime does not implement yet, mark it unsupported (next section) so it tracks as a
   SKIP instead of a FAIL.
4. Run `cargo test -p node_conformance` and `cargo run -p node_conformance` to confirm it is picked up
   and classified as you expect.

## Marking a test unsupported (the known-unsupported manifest)

There are two ways to record that the runtime cannot pass a test yet. Both produce a **SKIP** with a
reason and never evaluate the test body.

### 1. In-file directive — for a test whose *body* needs an unimplemented API

Put a single line anywhere in the file (by convention the first line):

```js
// CONFORMANCE: skip — node:child_process is not implemented in the Treaty runtime yet
```

The literal token is `CONFORMANCE: skip`; everything after the first `—` / `-` / `:` separator is
captured as the human-readable reason.

### 2. `known-unsupported.json` — for marking tests *without editing them*

`known-unsupported.json` is the central manifest of gaps — ideal for upstream Node tests vendored
verbatim that you do not want to edit. It is an object with one `"unsupported"` array; each entry has
a `"pattern"` (an exact case name **or** a glob over case names) and a `"reason"`:

```json
{
  "unsupported": [
    { "pattern": "crypto-*",        "reason": "node:crypto not implemented" },
    { "pattern": "worker-threads*", "reason": "worker_threads pending" }
  ]
}
```

Glob metacharacters in `pattern`: `*` matches any run of characters (including none) and `?` matches
exactly one character; everything else is literal and the **whole** case name must match. The first
matching entry wins, so a specific entry may precede a broad glob to override its reason. An in-file
directive takes precedence over the manifest when both apply.

Load it with `UnsupportedManifest::load_default()` (a missing file is treated as empty; a malformed
file is a hard error) and drive a corpus with `manifest.run_corpus(dir)`.

To **re-enable** a test once the runtime supports its API: delete its entry from
`known-unsupported.json` (and remove any in-file `// CONFORMANCE: skip` directive), then re-run the
harness — it will now execute and count toward the pass-rate.

## Tests

```sh
cargo test -p node_conformance
```

covers the skip-directive parser, the glob/name manifest matcher and loader, the per-module reporter
aggregation, the JSON report round-trip, and a full walk of the seed corpus.
