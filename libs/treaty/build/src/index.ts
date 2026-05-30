/**
 * @module
 *
 * Public API of `@treaty/build`: Angular CLI architect builders that make
 * `ng build` and `ng serve` run Treaty. The package's `builders.json` registers
 * two builders against the consuming workspace:
 *
 *   - `@treaty/build:application` — the `ng build` path. Compiles the project
 *     through the Treaty Rspack plugin (`@treaty/rspack`) and emits a federated
 *     host/remote bundle.
 *   - `@treaty/build:dev-server` — the `ng serve` path. Serves the same federated
 *     build with HMR.
 *
 * Both builders auto-wire Module Federation via `@treaty/module-federation`, so
 * the developer configures nothing in `angular.json` beyond pointing the target
 * at the Treaty builder. Treaty is a compiler, not a host: the builders only
 * assemble config and drive the bundler — all lowering lives in `@treaty/rspack`.
 *
 * The default exports of `./application` and `./dev-server` are the architect
 * `Builder` objects referenced by `builders.json`; the named exports here are the
 * typed option interfaces and the testable run functions.
 */

export { default as applicationBuilder, runApplicationBuild } from './application.js'
export { default as devServerBuilder, runDevServer } from './dev-server.js'

export { createTreatyRspackConfig } from './config.js'
export type { TreatyRspackConfig, TreatyRspackConfigInput } from './config.js'
export { toMfOptions } from './options.js'
export type { ApplicationBuilderOptions, DevServerBuilderOptions } from './options.js'
