/**
 * Copy non-TypeScript schematic assets into `dist/` after `tsc` runs.
 *
 * `tsc` only emits the compiled `.ts` files. The Angular schematics runtime
 * additionally needs, relative to the built factories:
 *   - `collection.json` at the package's `dist/` root (the `"schematics"`
 *     entry point), with its `$schema` rewritten to resolve from `dist/`;
 *   - each schematic's `schema.json`;
 *   - each schematic's `files/` template tree (`.template` + scaffolded files).
 *
 * Mirroring the `src/` layout into `dist/` keeps the relative factory/schema
 * paths in `collection.json` valid (`./ng-add/index.js`, `./ng-add/schema.json`).
 */

import { cp, mkdir, readFile, writeFile } from 'node:fs/promises'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const root = dirname(here)
const src = join(root, 'src')
const dist = join(root, 'dist')

await mkdir(dist, { recursive: true })

// collection.json: rewrite its dev-time $schema (which points up out of src/)
// to one that resolves from dist/, then write it to the dist root.
const collection = JSON.parse(await readFile(join(src, 'collection.json'), 'utf-8'))
collection.$schema =
	'../../../../node_modules/@angular-devkit/schematics/collection-schema.json'
await writeFile(join(dist, 'collection.json'), `${JSON.stringify(collection, null, 2)}\n`)

// Per-schematic schema.json files.
for (const name of ['ng-add', 'application', 'library']) {
	await mkdir(join(dist, name), { recursive: true })
	await cp(join(src, name, 'schema.json'), join(dist, name, 'schema.json'))
}

// Template trees for the generators.
for (const name of ['application', 'library']) {
	await cp(join(src, name, 'files'), join(dist, name, 'files'), { recursive: true })
}

console.log('schematics: copied collection.json, schema.json, and template files into dist/')
