/**
 * Side-effecting entry that registers the {@link ./ts-source-loader.mjs} resolver
 * hook, so a subsequent `--import` of this file lets the unit tests import the
 * package's TypeScript source (`src/**.ts`) directly under type-stripping.
 */

import { register } from 'node:module'
import { pathToFileURL } from 'node:url'

register(new URL('./ts-source-loader.mjs', pathToFileURL(import.meta.filename)))
