# @treaty/rolldown

A [Rolldown](https://rolldown.rs) plugin for Treaty authoring formats. It mirrors
[`@treaty/vite`](../vite): it lowers `.treaty`, `.tsx`, `.tjsx`, and Angular
`@Component` `.ts` files to Ivy JS via [`@treaty/compiler`](../compiler), and it
links published *partial*-compiled `@angular/*` libraries in `node_modules` to AOT
(`ɵɵngDeclare*` → `ɵɵdefine*`) via the shared, Rust-backed linker from
[`@treaty/ts-vite`](../../typescript/vite) — so a Rolldown bundle needs **no JIT**
and **no `@angular/compiler`**.

Treaty is a compiler, not a host. This package reimplements no lowering and no
linking: it calls the exact same addon-backed helpers `@treaty/vite` uses
(`createTreatyCompiler(...).transform` for authoring lowering, and
`linkPartialCode` / `isPartialModule` for partial-library linking), and only adds
the thin Rolldown wiring around them.

## Install

```sh
npm install -D @treaty/rolldown rolldown
```

`rolldown` is a peer dependency (`^1.0.0`).

## Usage

`treaty()` returns an **array** of Rolldown plugins (the authoring plugin plus the
Angular partial-declaration linker). Rolldown flattens nested plugin arrays, so it
drops straight into `plugins`:

```js
// rolldown.config.js
import { defineConfig } from 'rolldown'
import treaty from '@treaty/rolldown'

export default defineConfig({
  input: 'src/main.ts',
  plugins: [treaty()],
})
```

## Options

```ts
treaty({
  // Emit the compiler's v3 source map alongside lowered code. Default: true.
  sourceMap: true,

  // Split each extracted server fn into its own `<id>.server.js` output asset and
  // emit a `treaty-server-fns.json` manifest, keeping the server BODY out of the
  // client graph (the client follows a tiny RPC stub). Default: true.
  functionChunking: true,

  // Link published partial @angular/* libraries to AOT. Default: true.
  linkPartials: true,

  // Cold-build prewarm: batch-compile these owned files up front via the core's
  // `transformMany` so the per-module transforms become cache hits. Optional.
  prewarm: ['/abs/path/to/app.treaty'],
})
```

All other options are forwarded to the underlying `TreatyCompiler`
(see `TreatyCompilerOptions` from `@treaty/compiler`).

## How it maps to Rolldown's plugin API

Rolldown's plugin interface is Rollup-compatible, and the `@treaty/vite` plugin is
already a Rollup-native design with no Vite-internal *value* APIs, so the hooks
translate one-to-one:

| Concern                         | Hook(s)                                  |
| ------------------------------- | ---------------------------------------- |
| Cold-build prewarm              | `buildStart`                             |
| Own `.treaty`/`.tsx`/… imports  | `resolveId` (+ `this.resolve` `skipSelf`)|
| Server-fn virtual modules       | `resolveId` + `load` (`\0`-prefixed ids) |
| Lower authoring → Ivy JS        | `transform`                              |
| Emit server-fn body assets      | `this.emitFile({ type: 'asset' })`       |
| Watch-mode cache invalidation   | `watchChange`                            |
| Emit `treaty-server-fns.json`   | `generateBundle`                         |
| Link partial `@angular/*`       | `transform` (separate linker plugin)     |

The Vite-only pieces — the dev-server content-type middleware, the `/__server/*`
dev backend, the `optimizeDeps` esbuild prebundle linker, the `index.html`
JIT-script guard, and esbuild type-stripping — have **no Rolldown analogue**:
Rolldown is a bundler with no dependency prebundling, no dev server, and a native
TS/TSX parser, so a single Rollup-style `transform` covers both authoring lowering
and partial linking with nothing extra needed. In Vite the linker required three
plugins (prebundle linker + module-graph linker + HTML guard); here it is one
`transform` hook.
```
