/**
 * @module
 *
 * The Rspack/webpack loader entry for the file-routing virtual module. A loader
 * must be its OWN module (the bundler `require`s the loader path), so this file is
 * the thin default-export wrapper around {@link routesLoader} from
 * `./routes-virtual` (which holds the shared logic + the sentinel/test wiring).
 */

import { routesLoader } from './routes-virtual.js'

export default routesLoader
