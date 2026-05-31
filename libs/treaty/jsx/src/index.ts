import './jsx-types.js'
/**
 * @module
 *
 * Public entry of `@treaty/jsx`: the shipped ambient JSX types for Treaty's
 * selectorless, signal-aware, standalone JSX authoring format.
 *
 * Consuming projects do not import from here directly. Instead they load the
 * ambient `JSX` namespace one of three ways:
 *
 *  1. **`"types": ["@treaty/jsx"]`** in `tsconfig.json` — TypeScript resolves
 *     the package's `types` entry (this module) and picks up the ambient
 *     declarations via the side-effecting imports above.
 *  2. **`"jsxImportSource": "@treaty/jsx"`** with `"jsx": "react-jsx"` — the
 *     automatic runtime in `./jsx-runtime.ts` re-exports the `JSX` namespace so
 *     element typing resolves through `@treaty/jsx/jsx-runtime`.
 *  3. **The `@treaty/lsp` language server**, which auto-provides these types to
 *     every Treaty `.tsx` / `.tjsx` file so editors need no project config.
 *
 * The import above is side-effecting: `./jsx-types.js` carries no runtime
 * values, only `declare global { namespace JSX … }` (and it pulls in the
 * `declare global { namespace TreatyJsx … }` machinery). Importing it is what
 * installs those ambient namespaces. Re-exporting the runtime keeps
 * `import { jsx } from '@treaty/jsx'` working for tooling that reaches the
 * package root.
 */

export { jsx, jsxs, jsxDEV, Fragment } from './jsx-runtime.js'
export type { JsxProps } from './jsx-runtime.js'
