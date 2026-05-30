# Phase 1b — JS/Angular upgrade plan (from `angular-upgrade-research` workflow)

## Decisions (flag for user override)
1. **Angular → `21.2.15` stable, NOT 22.** Angular 22 is not on npm as stable yet (only
   `22.x-next` prereleases — which is what we cloned as the *port reference*). 21.2.15 is the latest
   installable stable and is what lets the workspace build green. **22 is a fast-follow re-bump**
   when it ships stable (~early June 2026); the render3 *port* already targets 22 internals via
   `tools/angular-ref`, so the compiler work is unaffected.
2. **Remove Nx entirely.** The repo migrated to **moon** (`.moon/`, `moon.yml`); there is no
   `nx.json` or `project.json`. All `@nx/*`, `nx`, `@nrwl/*`, plus the unmaintained blockers
   `@nx-bun/nx` and `@monodon/rust`, are vestigial. Removing them avoids a 5-major Nx migration and
   matches the repo's stated direction. (Moon currently defines almost no tasks — green gate runs on
   `cargo` for Rust and `vite build` for the REPL.)

## Exact version targets (root + per-package `package.json`)
| package | target | note |
|---|---|---|
| `@angular/{core,common,compiler,animations,forms,router,platform-browser,platform-browser-dynamic}` | `^21.2.15` | lockstep |
| `@angular/{compiler-cli,language-service}` | `^21.2.15` | dev |
| `@angular/cli`, `@angular-devkit/{build-angular,core,schematics}`, `@schematics/angular` | `^21.2.13` | CLI train |
| `typescript` | `~5.9.0` | compiler-cli peer 5.9..<6.1 (was ~5.3) |
| `zone.js` | `~0.16.0` | zoneless-capable (was ~0.14) |
| `rxjs` | `~7.8.0` | unchanged |
| `tslib` | `^2.3.0` | unchanged |
| `vite` | `~7.3.34` | Node 20.19+/22.12+ required |
| `vitest`,`@vitest/ui`,`@vitest/coverage-v8` | `^4.1.7` | lockstep (if vitest used) |
| `jest-preset-angular` | bump to Angular-21-compatible line | currently 13.1.4 |
| `@angular-eslint/*` | `^21` (or 20) | was 17 |
| `@typescript-eslint/*` | `^8` | was 6 |
| `eslint` | `^9` (flat config) | was 8.48 |
| `ng-packagr` | `^21` | was 17.3 (libs/treaty/edenclient) |
| **remove** | `nx`, `@nx/*`, `@nrwl/*`, `@nx-bun/nx`, `@monodon/rust` | vestigial / moon is runner |

Also migrate the 17.x stragglers: [libs/treaty/edenclient](../libs/treaty/edenclient/) and
[libs/typescript/compiler](../libs/typescript/compiler/) `@angular/*` 17.x → 21.2.15.
Node engines: require `20.19+ || 22.12+ || 24+`.

## `@angular/compiler` API edits — [treat-to-ivy.ts](../apps/repl/src/tools/treaty-sfc/treat-to-ivy.ts) + [libs/typescript/compiler](../libs/typescript/compiler/src/lib/compiler.ts)
`compileComponentFromMetadata` call (≈lines 373–423):
1. `makeBindingParser(DEFAULT_INTERPOLATION_CONFIG)` → **`makeBindingParser()`** (no args). In v21/22 it
   takes only `selectorlessEnabled`; the old arg wrongly feeds that boolean.
2. **Delete `interpolation`** field (removed from `R3ComponentMetadata` after v18 → excess-property error).
3. **Delete `fullInheritance`** field (removed from `R3DirectiveMetadata` after v18).
4. **Add required fields** (v19+): `relativeTemplatePath: null`, `hasDirectiveDependencies: false`.
   (v22-only, omit on 21.2: `foreignImports: null`, `controlCreate: null`, `legacyOptionalChaining: false`.)
5. **Fix `rawImports`**: it's now an `Expression`, not `string[]`. The `as any` hides a bug — delete the
   `rawImports` line (deps are already injected as a `LiteralArrayExpr` onto `out.expression.args` ≈line 425),
   or set it to a `LiteralArrayExpr` of `WrappedNodeExpr`.

Unchanged/safe: `parseTemplate(html, id)`, the `(meta, constantPool, bindingParser)` arity and
`out.expression`/`out.statements`, `R3InputMetadata` (signal inputs keyed by `isSignal:true` + `required`),
`host{...}`, `defer{dependenciesFn, mode:1}`, `declarationListEmitMode:0`, `ViewEncapsulation.Emulated`,
`ConstantPool`, `WrappedNodeExpr`, `LiteralArrayExpr`, `DEFAULT_INTERPOLATION_CONFIG`.

## Verification (revised green gate — no Nx)
- Rust: `cargo build --workspace && cargo test` (primary).
- JS: `bun install` clean; REPL `vite build` succeeds; `tsc -p` on libs passes.
- REPL renders a `.treaty` component (use `/run` skill).
