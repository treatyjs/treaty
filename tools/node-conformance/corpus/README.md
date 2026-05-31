# Node conformance corpus

Each `.js` file here is a Node `test/parallel`-style conformance test, run through the Treaty
runtime's `JsRuntime::with_node_compat` by the runner in `../src/runner.rs`.

## Naming = module grouping

Files are flat (the runner walks this directory non-recursively) and named `<module>-<topic>.js`.
The reporter buckets cases into the per-module pass-rate table by the segment before the first `-`
(`module_of`), so `fs-sync-roundtrip`, `fs-constants-stat` and `fs-promises-roundtrip` all roll up
under the `fs` module. Curated coverage of the currently-implemented surfaces:

- `buffer-encodings` — `Buffer.from`/`alloc`/`concat`/`compare`/`equals`/`isBuffer`; utf8/hex/base64.
- `console-log` — the `node:console` writer surface (`log`/`error`/`warn`/`info`).
- `events-emitter` — `EventEmitter` `on`/`emit`/`once`/`removeListener`/`listenerCount` + `error` throw.
- `fs-sync-roundtrip` — sync `write`/`read`/`exists`/`append`/`copy`/`rename`/`unlink`/`rmdir`.
- `fs-constants-stat` — `fs.constants`, `statSync` (size/`isFile`/`isDirectory`), `readdirSync`.
- `fs-promises-roundtrip` — async `node:fs/promises` round-trip + a rejection (via `assert.rejects`).
- `os-info` — `platform`/`arch`/`type`/`release`/`hostname`/`tmpdir`/`homedir`/`EOL`/`cpus`/`endianness`.
- `path-join-resolve` — `join`/`resolve`/`dirname`/`basename`/`extname`/`normalize`/`isAbsolute`/`sep`/`posix`/`win32`.
- `process-argv-env` — `argv`/`env`/`cwd`/`platform`/`version`/`pid`/`nextTick`.
- `timers-ordering` — `setTimeout`/`clearTimeout`/`setInterval`/`clearInterval`/`queueMicrotask` ordering.
- `url-parse-format` — the legacy functional `url.parse()`/`format()`/`fileURLToPath()` API.
- `util-format-types` — `format`/`inspect`/`isDeepStrictEqual`/`types`/`promisify`.

## The embedded harness shim

The runner hands each file's bytes straight to the runtime's `eval`; there is no seam to inject a
shared prelude, and that `eval` wrapper makes Node's lazy *globals* (`process`, `Buffer`,
`setTimeout`, `TextEncoder`, `URL`, `console`, `queueMicrotask`, …) unreachable — a test can only
reach a builtin through `require(...)`.

So every executed test begins with a byte-identical copy of the harness shim (the block delimited by
`// === treaty node-conformance harness shim (begin/end) ===`), whose canonical source is
`../src/harness_shim.rs::ASSERT_SHIM`. The shim provides a Node-style `assert` (`ok`,
`strictEqual`, `notStrictEqual`, `deepStrictEqual`, `throws`, `rejects`, `fail`) plus a tiny
`test()`/`it()`/`describe()` runner, all built on `require("node:util")` only. A crate test
(`harness_shim::tests::every_corpus_file_embeds_the_canonical_shim`) fails the build if any executed
test's embedded copy drifts from the canonical source.

## Pass / skip rules

- A test **passes** when the file evaluates to completion (and the post-eval event-loop drain
  settles) without throwing. `test()`/`assert.*` throw on failure, exactly like Node's own tests; an
  `async test()` returns a promise whose rejection the drain surfaces as a failure.
- A test is **tagged SKIP** by putting the directive `CONFORMANCE: skip` on a line (by convention the
  first), with the reason after the first `—`, `-`, or `:` separator:

  ```js
  // CONFORMANCE: skip — node:child_process is not implemented in the Treaty runtime yet
  ```

  A skipped file is never evaluated, so it may reference unimplemented or unreachable APIs freely.
  Skips here are not "missing features" in the loose sense — each names the precise gap, e.g.
  `text-encoding-utf8` / `url-whatwg-classes` document that `TextEncoder`/`TextDecoder` and the
  WHATWG `URL`/`URLSearchParams` classes are global-only and so unreachable from the harness eval
  scope. Tests can also be skipped centrally (by name/glob, without editing the file) via
  `../known-unsupported.json`.

Seed the corpus only against currently-implemented surfaces: `require` + the `node:` builtins `fs`,
`fs/promises`, `path`, `process`, `buffer`, `os`, `util`, `events`, `console`, `timers`, `url`, text
encoding, and `fetch` (plus the event loop). Anything else belongs behind a SKIP tag.
