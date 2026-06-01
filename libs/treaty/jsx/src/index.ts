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

/**
 * Author-facing helper types for the Treaty authoring surface. These alias into
 * the ambient `TreatyJsx` machinery so authors can annotate the formats Treaty
 * lowers — a directive, its host spec, and a pipe — and get editor type-checking
 * and completion WITHOUT hand-declaring anything:
 *
 *  - {@link Directive} — a directive authored as a function returning a
 *    {@link HostSpec} (or a decorated class). `Directive<Input>` types the value
 *    a `use:<name>={input}` application passes.
 *  - {@link DirectiveFn} — the function form on its own (`(input?) => HostSpec`).
 *  - {@link HostSpec} / {@link HostBindings} — the `{ host: { … } }` object a
 *    directive function returns, keyed by Angular host microsyntax.
 *  - {@link Pipe} — a pipe authored as a transform function
 *    (`(value, ...args) => out`); {@link PipeTransform} is the class form's
 *    Angular-shaped contract.
 *
 * They are pure type aliases (no runtime value), re-published so a `.tsx` /
 * `.tjsx` / `.treaty` author can `import type { Directive, Pipe } from '@treaty/jsx'`.
 */
export type Directive<A = void> = TreatyJsx.Directive<A>
export type DirectiveFn<A = void> = TreatyJsx.DirectiveFn<A>
export type DirectiveInput<D> = TreatyJsx.DirectiveInput<D>
export type HostSpec = TreatyJsx.HostSpec
export type HostBindings = TreatyJsx.HostBindings
export type Pipe<
	In = unknown,
	Out = unknown,
	Args extends readonly unknown[] = readonly unknown[],
> = TreatyJsx.Pipe<In, Out, Args>
export type PipeTransform = TreatyJsx.PipeTransform
