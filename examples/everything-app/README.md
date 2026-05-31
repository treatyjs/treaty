# @treaty-examples/everything-app

A single example that exercises **every Treaty authoring capability** in one
place. These are showcase **sources**: each file is compiled and verified
through the real Treaty Rust/OXC compiler (see `verify.mjs`), but the example is
not a running server.

Treaty defaults to **selectorless + standalone + signal** — the compiler fills
those in — so none of the components below carry a `selector`, `standalone:
true`, or change-detection boilerplate. Treaty is a compiler, not a host.

## What each file demonstrates

### Components

| File | Authoring form | Shows |
| --- | --- | --- |
| `src/components/todo-list.treaty` | `.treaty` single-file component | A top **macro fence** (```` ``` ````) that computes **render-time data** at compile time (zero runtime cost); a **TypeScript-by-default** body (no `<script>` wrapper); the view as **interleaved tag-HTML with NO `<template>` wrapper** (HTML detected by its tags); and a `<style lang="scss">` block with nesting + SCSS variables. Calls the **API**-transport server fns. |
| `src/features/greeter/greeter.treaty` | `.treaty` single-file component | A second SFC that leans into **TS / HTML / TS / HTML interleave** (no `<template>`): a top **macro fence**, a TS body, an inline **`server { ... }`** block (the block-style server-fn marker), more TS after it, then the tag-HTML view and a `<style lang="scss">` block. Shows the `server { }` extraction marker. |
| `src/components/counter.tsx` | JSX (`.tsx`) | A **lowercase** component (`counter`) returning JSX; **`use:class`** and a custom **`use:highlight`** directive; **`{count()}`** signal interpolation; an **`onClick`** handler; an **`items.map(...)`** list; and a conditional that lowers to an Ivy **`@if`/`@else`**. Calls the **WebSocket**-transport server fn. |
| `src/features/greeter/greeting-card.tjsx` | JSX (`.tjsx`) | A **FUNCTION component** (`greetingCard`) — functions are first-class components; **lowercase** name; **signal** locals (`name`, `log`); lowercase **`class`**; a **`use:autofocus`** directive; and Angular **`@if`/`@for`** control-flow blocks written **directly in the JSX** (not a `.map()`/ternary lowering). Declares an **inline `$$`-marked server fn** (`loadGreeting$$`) in the same file — a separate `.server.ts` is optional, not required. |
| `src/components/log-viewer.component.ts` | Plain Angular `@Component` (`.ts`) | Base Angular — a decorated class — **without a `selector`** (selectorless), using **signals** + **`@if`/`@for`** control flow. Calls the **Stream**-transport server fn. Shows that base-Angular benefits from the same compiler. |
| `src/features/greeter/greeter-page.component.ts` | Plain Angular `@Component` (`.ts`) | A selectorless decorated class — **no `selector`/`standalone`/CD boilerplate** — default-exported as the **lazy `greeter` route**'s `loadComponent`. It hosts the `.treaty` and `.tjsx` components **selectorlessly** (auto-import by value), so one federated remote shows all three surfaces interop. |

### Server functions (three transports, all three markers)

The function **bodies never enter the Ivy/client bundle**; only the typed call
site in a component does (it compiles to the resource/Eden binding). Treaty
recognizes server fns three ways — all shown here: a `'use server'` prologue, an
inline `server { ... }` block, and a `$$`-suffixed name.

| File | Transport | Marker |
| --- | --- | --- |
| `src/server/todos.server.ts` | **API** (request/response) | `'use server'` prologue; exported `async` functions. |
| `src/server/presence.ws.ts` | **WebSocket** (duplex/push) | `'use websocket'` prologue; `ws`-prefixed handler (`wsPresence`). |
| `src/server/logs.stream.ts` | **Stream** (server-push, many values) | `async function*` (async generator) under `'use server'`. |
| `src/features/greeter/greeting-card.tjsx` (inline) | **API** (request/response) | An **inline `$$`-suffixed** fn (`loadGreeting$$`) declared in the `.tjsx` component itself — the naming-convention marker, no sidecar `.server.ts` needed. |
| `src/features/greeter/greeter.treaty` (inline) | **API** (request/response) | An inline **`server { ... }`** block inside the `.treaty` SFC — the block-style marker. |

### Routing + Module Federation

| File | Shows |
| --- | --- |
| `src/routes/app.routes.ts` | One eager route plus three **lazy** feature routes (two `loadComponent`, one `loadChildren`). Treaty's `deriveExposesFromRoutes` turns every lazy boundary into an independently deployable **Module Federation remote** with no hand-written `exposes` map. |
| `src/features/dashboard/dashboard.component.ts` | A lazy single-component remote (`loadComponent`). |
| `src/features/profile/profile.routes.ts` | A lazy child-routes remote (`loadChildren`) with nested routing. |
| `src/features/profile/profile.component.ts`, `profile-settings.component.ts` | The Profile feature's screens (one decorated class per file). |
| `src/features/greeter/greeter-page.component.ts` | A lazy single-component remote (`loadComponent`, auto-exposed as `./routes/greeter`) that **calls an extracted server fn** and hosts the `.treaty` + `.tjsx` surfaces — showing server-fn extraction and federation together. |

Running the route graph through `deriveExposesFromRoutes` yields:

```json
{
  "./routes/index": "./src/app/index",
  "./routes/dashboard": "./src/app/dashboard",
  "./routes/profile": "./src/app/profile",
  "./routes/greeter": "./src/app/greeter"
}
```

## Build configs: one compiler, every bundler

Treaty is a **compiler, not a host** — it never starts a server. Each config
below only contributes the Treaty plugin (which lowers the authoring formats to
Ivy JS via `@treaty/compiler` → the Rust addon) and, for the app builds, the
**auto-generated Module Federation** wiring. The developer runs the bundler.

| File | Plugin | Module Federation | Run |
| --- | --- | --- | --- |
| `vite.config.ts` | `@treaty/vite` `treatyWithFederation` | auto-MF on (returns the Treaty plugin **and** the `@module-federation/vite` plugin) | `vite build` / `vite` |
| `rspack.config.ts` | `@treaty/rspack` `TreatyRspackPlugin` | auto-MF on by default (adds `@module-federation/enhanced` `ModuleFederationPlugin`) | `rspack build -c rspack.config.ts` |
| `rsbuild.config.ts` | `@treaty/rsbuild` `pluginTreaty` | wired via the `@treaty/module-federation` adapter (`generateMfConfig` → `toRspackModuleFederation`) | `rsbuild build` / `rsbuild dev` |
| `rslib.config.ts` | `@treaty/rslib` `defineTreatyLib` | none — a library is consumed, not federated | `rslib build -c rslib.config.ts` |

In every app build the federation `exposes` map is **derived from the route
graph** by `deriveExposesFromRoutes` (run inside `@treaty/module-federation`) —
the developer writes no `exposes` by hand. Passing `routes` is the whole config.
The three lazy routes (`dashboard`, `profile`, `greeter`) become independently
deployable remotes; the eager index route stays in the host.

> The bundler peers (`vite`, `@rspack/core`, `@rsbuild/core`, `@rslib/core`, the
> `@module-federation/*` packages) are **optional peer dependencies** — the
> developer's build provides them. The configs reference those peers
> *structurally* (the same discipline the Treaty plugins use), so they
> `tsgo`-typecheck in this repo without the bundlers installed. `peers.d.ts`
> restates the minimal structural shapes of the optional peers this example
> touches.

### Module Federation host / remote pair

`federation/` shows a host consuming a remote with **zero-config auto-MF**:

| File | Shows |
| --- | --- |
| `federation/remote.config.ts` | The `profile`-style remote. Calls `deriveExposesFromRoutes(appRoutes)` directly — **no hand-written `exposes`** — so every lazy route is exposed automatically; eager routes are skipped. |
| `federation/host.config.ts` | The host. Declares the `profile` remote it consumes (`name@entryUrl`) and shares the Angular runtime as eager singletons. The host also auto-exposes its **own** lazy routes, so an app is a host and a remote at once. |
| `federation/routes-bridge.ts` | One documented place that views the app's Angular `Routes` through Treaty's structural `RouteLike[]` type (the configs import `appRoutes` from here). |

### Federation deploy manifest (deploy / rollback one module)

`federation/federation.manifest.ts` builds the versioned deploy manifest with
`@treaty/federation-deploy` `buildManifest` (one `{ version, url }` entry per
federated module: the host plus each auto-exposed lazy route), serializes it to
canonical JSON, and demonstrates rolling a **single** module forward and back
without touching the rest of the app:

```ts
// DEPLOY just ./routes/profile to v1.1.0 — every other module stays at 1.0.0:
const deployed = setModuleVersion(manifest, './routes/profile', '1.1.0', {
  url: 'https://cdn.example.com/routes/profile/1.1.0/remoteEntry.js',
})
// ROLLBACK that one module to the prior good version — nothing else moves:
const rolledBack = rollbackModule(deployed, './routes/profile', '1.0.0', {
  url: 'https://cdn.example.com/routes/profile/1.0.0/remoteEntry.js',
})
```

`createTreatyMfRuntimePlugin(manifest)` (also exported there) is the
`@module-federation/enhanced` runtime plugin the host registers; it resolves
each remote's url+version from the manifest **at load**, so a manifest flip
(deploy/rollback) takes effect on the next load with no rebuild.

## Authoring notes

- The Treaty JSX dialect (lowercase elements, `use:` directives, `class` over
  `className`, `{signal()}` interpolation) is typed by the **shipped
  `@treaty/jsx` ambient types** — `tsconfig.json` sets
  `"jsxImportSource": "@treaty/jsx"`, so the `.tsx`/`.tjsx` sources are
  `tsgo`-clean with no hand-copied `treaty-jsx.d.ts` shim in the example.
- The JSX front-end authors components as **functions** returning JSX. It can
  express conditionals two ways: as JSX expressions (ternary / `&&`) that lower
  to Ivy `@if` (`counter.tsx`), **or** with the native `@if`/`@for` control-flow
  blocks written directly in the JSX (`greeting-card.tjsx`). `.treaty` and `.ts`
  templates use the same native `@if`/`@for` block syntax.
- Importing a `.treaty`/`.tjsx` authoring file from a plain `.ts` host resolves
  through the ambient `*.treaty` / `*.tjsx` module shapes in `peers.d.ts` (the
  compiler lowers them to real components at build time); intrinsic-element
  typing inside the JSX still comes from the shipped `@treaty/jsx` types.
- Keep sources **ASCII** in template/macro regions: the macro/template lexer
  indexes by byte offset, so non-ASCII punctuation in those areas is avoided.

## Verifying

```bash
# Type-check (uses the Native-Preview tsc, not tsc):
node_modules/.bin/tsgo --noEmit -p examples/everything-app/tsconfig.json

# Lint (sources + build configs):
node_modules/.bin/oxlint examples/everything-app/src examples/everything-app/*.config.ts examples/everything-app/federation

# Compile every source through the real Treaty Rust addon:
bun examples/everything-app/verify.mjs
```

The build configs themselves are part of the type-check above (the
`tsconfig.json` `include` covers `*.config.ts` and `federation/`), so
`tsgo` proves every bundler config is wired to the Treaty plugins correctly
without needing the bundlers installed.

## A note on the REPL plugin-output viewer

A self-contained companion lives at
`apps/repl/src/app/plugin-output-viewer.component.ts`. It drives the
`@treaty/compiler` `transform` / `transformMany` seam (the same one every
bundler plugin uses) and renders, per input file, the emitted Ivy JS, any
extracted server module, and the `sideEffects` tree-shaking hint — a faithful
preview of what the Treaty bundler plugins emit for these sources.
