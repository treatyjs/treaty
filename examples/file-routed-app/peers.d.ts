/// <reference path="../../libs/treaty/compiler/dist/ambient.d.ts" />
/// <reference path="../../libs/treaty/jsx/dist/ambient.d.ts" />

/**
 * Ambient module declarations for this example.
 *
 * The two triple-slash references above bring in the authoring-format module
 * shims from the packages that OWN each format — `*.treaty` from
 * `@treaty/compiler` (its `./ambient` entry) and `*.tjsx` / `*.tsx` from
 * `@treaty/jsx` — so the generated route module's lazy `import('…/foo.treaty')`
 * loaders typecheck without a per-app `declare module` block. This workspace
 * consumes the libs from their built `dist`, so the references target the
 * shipped `dist/ambient.d.ts` files directly; an app that installs the packages
 * as real dependencies uses the package-name form instead
 * (`/// <reference types="@treaty/compiler/ambient" />`) or `compilerOptions.types`.
 *
 * Treaty is a compiler, not a host: nothing here runs; these declarations only
 * keep the authoring-time route graph type-clean.
 */
export {}
