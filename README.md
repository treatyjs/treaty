# Treaty

A Rust/OXC Angular compiler and Node-compatible runtime. Treaty compiles
Angular **directly to Ivy** in Rust (no `tsc`, no `@angular/compiler` at runtime),
ships its own Node-compatible runtime, and lets you author with `.treaty` SFCs or
JSX-flavored Angular — signals by default.

> Branch: `migration/v22-oxc133` · OXC 0.133 · Angular 22 targets.
> Detailed state: [`migration/STATUS.md`](migration/STATUS.md).

## Features

| Area | What | Status |
| --- | --- | --- |
| Compiler | `treaty_ivy` — direct-to-Ivy in Rust (4-crate carve: core/template/decorators/facade), `DecoratorCompiler` registry | ✅ 170/185 runnable Angular golden parity (91.9%, climbing) |
| Authoring → Ivy | `.treaty` SFC + JSX-flavored Angular authoring plugins → `treaty_ivy` | ✅ |
| Decorators / DI | `@Component/@Directive/@Injectable/@Pipe`, constructor DI (`ɵɵinject`/`ɵɵdirectiveInject` + `InjectFlags`), queries, `@Input({alias,transform})`, host bindings/styling, `signals: true` | ✅ |
| Consuming Angular libs | Built-in **Angular Linker**: partial `ɵɵngDeclare*` → AOT `ɵɵdefine*`, no JIT, dev + prod | 🏗️ in progress (bundler wiring next) |
| Runtime | Node-compatible Nova runtime, module-granular `node:` builtins, real `node:tls`/`node:https` over rustls (ring), offline-clean | ✅ 483 tests; conformance 51/0 + corpus 68/0 |
| SSR / SSG | Server render + static generation cores (Rust-first deterministic logic) | 🏗️ |
| File-based routing | `treaty_file_routing` crate + CLI; real-engine route generation | ✅ (`examples/file-routed-app`) |
| Server functions | Inline by default (`server{}` / `'use server'` / `$`), backend-agnostic (axum default, Elysia opt-in); bodies excluded from client source maps | ✅ |
| Source maps | v3 maps (Ivy JS ↔ authoring source) threaded through addon + bundler plugins | ✅ |
| Bundler / NAPI | `compileComponent`, `compileComponentSource`, `compileTreatyFile`, `compile`, `compileMany`, `runMacro` (`linkPartial` lands with the linker) | ✅ |
| Tooling | Typecheck **tsgo**, lint **oxlint**, no `tsc` | ✅ |
| Build → boot e2e | Real vite build + boot of `everything-app` / `file-routed-app` | ⏳ queued (blocked on linker) |

Legend: ✅ done · 🏗️ in progress · ⏳ queued.

## Examples

- [`examples/everything-app`](examples/everything-app) — broad feature coverage.
- [`examples/file-routed-app`](examples/file-routed-app) — file-routing engine demo.

## Status

See [`migration/STATUS.md`](migration/STATUS.md) for the full per-workstream
breakdown (compiler parity, the Angular linker, runtime conformance, file
routing, examples, tooling) and the prioritized roadmap.
