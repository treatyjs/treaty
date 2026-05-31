/**
 * @module
 *
 * Server-function chunking for `@treaty/compiler`.
 *
 * The Rust authoring front-end hands the seam ONE concatenated `serverModule`
 * blob (a runnable backend: a shared preamble, every server fn declared
 * verbatim, then a `…/__server/<name>` route per fn, then a listen/router
 * tail). FUNCTION CHUNKING decomposes that blob into one {@link ServerFnChunk}
 * per exported server fn so a bundler can code-split each fn into its own
 * separately-loadable chunk — keeping the fn BODY out of the client bundle and
 * leaving only a per-fn client binding behind.
 *
 * The split is purely textual and backend-agnostic: it anchors on the
 * `…/__server/<name>` route registrations every backend emits (axum, Elysia,
 * Express) to discover the exported server-fn names, then partitions the blob
 * into a shared preamble plus a per-fn slice (the fn's verbatim declaration and
 * its route handler). Each fn's chunk `code` is `<preamble><fn slice>`, so the
 * fn body is wholly contained in its chunk and never leaks across fns.
 *
 * Framework-agnostic: no bundler is imported here.
 */

import { contentHash } from './cache.js'
import type { ServerFnChunk, TransformResult } from './types.js'

/**
 * One manifest entry: the author's export name for a server fn plus the stable
 * chunk reference a bundler should emit it under. Keyed in the manifest by the
 * fn's stable {@link ServerFnChunk.id}.
 */
export interface ServerFnManifestEntry {
	readonly exportName: string
	readonly chunkRef: string
}

/** The server-fn manifest: stable chunk id -> its export name + chunk ref. */
export type ServerFnManifest = Record<string, ServerFnManifestEntry>

/**
 * Match a `…/__server/<name>` route literal in any of the forms the backends
 * emit it (`'/__server/save'`, `"/__server/save"`, `` `/__server/save` ``,
 * `.route("/__server/save"`). The `/__server` prefix mirrors `SERVER_ROUTE_PREFIX`
 * in the Rust backend plugins; the capture group is the fn name (a JS identifier),
 * used as the anchor for discovering exported server-fn names in the blob.
 */
const ROUTE_RE = /["'`]\/__server\/([A-Za-z_$][\w$]*)["'`]/g

/**
 * Derive the stable chunk id for a server fn: `<file>#<fnName>` hashed to a
 * short hex token (prefixed so it reads as a chunk identity, not a raw hash).
 * Stable across builds for the same file + fn, independent of the fn's body.
 */
export function serverFnChunkId(fileId: string, fnName: string): string {
	return `srvfn_${contentHash(`${fileId}#${fnName}`)}`
}

/**
 * Discover the exported server-fn names in `serverModule`, in first-seen order,
 * de-duplicated. Names come from the `…/__server/<name>` route registrations.
 */
function discoverServerFnNames(serverModule: string): string[] {
	const names: string[] = []
	const seen = new Set<string>()
	ROUTE_RE.lastIndex = 0
	let m: RegExpExecArray | null
	while ((m = ROUTE_RE.exec(serverModule)) !== null) {
		const name = m[1]
		if (name && !seen.has(name)) {
			seen.add(name)
			names.push(name)
		}
	}
	return names
}

/** A half-open `[start, end)` byte range within the server module. */
interface Range {
	readonly start: number
	readonly end: number
}

/**
 * The candidate top-level declaration identifiers that belong to a server fn
 * named `name`, across the backends: the author's bare name (JS/TS backends
 * declare the fn verbatim, e.g. `function save`), the axum handler form
 * (`__server_<name>`) and its WebSocket socket fn (`__server_<name>_socket`),
 * and the axum request struct (`<Pascal>Request`). Matching any of these as a
 * top-level declaration contributes that declaration to the fn's chunk.
 */
function declIdentifiers(name: string): string[] {
	const pascal = name.charAt(0).toUpperCase() + name.slice(1)
	return [name, `__server_${name}`, `__server_${name}_socket`, `${pascal}Request`]
}

/**
 * Locate every top-level declaration STATEMENT that belongs to the server fn
 * `name` and return their `[start, end)` ranges. A declaration is `function id`,
 * `async function id`, `export …`, `const|let|var id =`, a Rust `fn id`/`pub …
 * fn id`, or a Rust `struct id` (for the generated request struct) — for any of
 * the {@link declIdentifiers} candidates. Empty when none are present.
 */
function declRanges(serverModule: string, name: string): Range[] {
	const ranges: Range[] = []
	for (const id of declIdentifiers(name)) {
		const esc = escapeRegExp(id)
		const decl = new RegExp(
			`^[\\t ]*(?:export\\s+)?(?:pub\\s+)?(?:async\\s+)?(?:function|fn|const|let|var|struct)\\s+${esc}\\b`,
			'm'
		)
		const m = decl.exec(serverModule)
		if (m === null) continue
		// Skip a prefix-name false positive: `save` must not match inside the line
		// that declares `__server_save` (the regex's `\b` already prevents this,
		// since the identifier is delimited), so each candidate is matched on its
		// own. Record the statement range.
		ranges.push({ start: m.index, end: statementEndFrom(serverModule, m.index) })
	}
	return ranges
}

/**
 * Locate the route registration STATEMENT that targets `name` and return its
 * `[start, end)` range: from the start of the line carrying the
 * `…/__server/<name>` literal through the end of that statement (brace/paren
 * balanced, so a `{ … }` handler body never ends the slice early). Returns
 * `null` when the route is not found.
 */
function routeRange(serverModule: string, name: string): Range | null {
	const anchor = new RegExp(`["'\`]\\/__server\\/${escapeRegExp(name)}["'\`]`)
	const m = anchor.exec(serverModule)
	if (m === null) return null
	const lineStart = serverModule.lastIndexOf('\n', m.index) + 1
	return { start: lineStart, end: statementEndFrom(serverModule, m.index) }
}

/**
 * Walk forward from `from` to the end of the enclosing statement: balance `()`,
 * `{}`, and `[]` (skipping string/template/comment content), then consume a
 * trailing `;` and the rest of the line. Returns an index at/after `from`.
 */
function statementEndFrom(src: string, from: number): number {
	let depth = 0
	let i = from
	let opened = false
	const len = src.length
	while (i < len) {
		const ch = src[i]!
		if (ch === '"' || ch === "'" || ch === '`') {
			i = skipString(src, i, ch)
			continue
		}
		if (ch === '/' && src[i + 1] === '/') {
			i = skipLineComment(src, i)
			continue
		}
		if (ch === '/' && src[i + 1] === '*') {
			i = skipBlockComment(src, i)
			continue
		}
		if (ch === '(' || ch === '{' || ch === '[') {
			depth++
			opened = true
		} else if (ch === ')' || ch === '}' || ch === ']') {
			depth--
			if (opened && depth <= 0) {
				i++
				// A `(params)`/`(args)` closing at depth 0 only ends the statement if
				// no `{ … }` block body follows: a fn signature continues into its
				// block — `function f(a) { … }`, an arrow `(a) => { … }`, or a Rust
				// `fn f(a) -> T { … }` — whereas a route call `app.post(…)` ends here.
				// Continue when a `{` precedes the next statement terminator.
				if (blockBodyFollows(src, i)) continue
				break
			}
		} else if (ch === ';' && depth <= 0) {
			i++
			break
		}
		i++
	}
	// Consume a trailing `;` and the rest of the line so the slice is clean.
	if (src[i] === ';') i++
	while (i < len && src[i] !== '\n') i++
	if (src[i] === '\n') i++
	return i
}

/**
 * Whether a `{ … }` block body follows position `i` before the statement ends.
 * Scans forward over the signature tail a fn may carry between its `)` and body
 * (`=> `, a Rust `-> Type`, `where` clauses) and returns true if a `{` is seen
 * before a `;` or a clear new-statement boundary. Used to decide whether a `)`
 * closing at depth 0 ends the statement (route call) or continues into a body.
 */
function blockBodyFollows(src: string, i: number): boolean {
	let j = i
	const len = src.length
	while (j < len) {
		const ch = src[j]!
		if (ch === '{') return true
		if (ch === ';' || ch === ')' || ch === '}' || ch === ',') return false
		// A blank line with no continuation token means the statement ended.
		if (ch === '\n' && src[j + 1] === '\n') return false
		j++
	}
	return false
}

/** Advance past a string/template literal starting at `start` (its quote char). */
function skipString(src: string, start: number, quote: string): number {
	let i = start + 1
	const len = src.length
	while (i < len) {
		const ch = src[i]!
		if (ch === '\\') {
			i += 2
			continue
		}
		if (ch === quote) return i + 1
		i++
	}
	return len
}

/** Advance past a `// …` line comment starting at `start`. */
function skipLineComment(src: string, start: number): number {
	let i = start + 2
	while (i < src.length && src[i] !== '\n') i++
	return i
}

/** Advance past a `/* … *\/` block comment starting at `start`. */
function skipBlockComment(src: string, start: number): number {
	let i = start + 2
	const len = src.length
	while (i < len) {
		if (src[i] === '*' && src[i + 1] === '/') return i + 2
		i++
	}
	return len
}

/**
 * Split a single `serverModule` blob into one slice per exported server fn.
 *
 * Backends emit the blob as a shared preamble, then every fn declared verbatim,
 * then a `…/__server/<name>` route per fn — i.e. a fn's declaration and its
 * route are TWO disjoint statements that may not be adjacent. So per fn we take
 * exactly those two ranges (declaration statement + route statement) and join
 * them; this keeps each fn's body wholly within its own slice and never pulls in
 * a neighbouring fn's declaration or route.
 *
 * `preamble` is everything before the FIRST fn declaration (imports/`use`/app
 * setup) — shared by every chunk so each is independently coherent. When no
 * `…/__server/<name>` routes are discoverable the fn list is empty and the whole
 * blob is the preamble, so the caller falls back to a single chunk.
 */
interface ServerModulePartition {
	readonly preamble: string
	readonly fns: readonly { readonly name: string; readonly slice: string }[]
}

function partitionServerModule(serverModule: string): ServerModulePartition {
	const names = discoverServerFnNames(serverModule)
	if (names.length === 0) return { preamble: serverModule, fns: [] }

	// Per fn, gather the byte ranges that belong to it: every declaration piece
	// that carries its body (the verbatim fn for JS/TS backends; the
	// `__server_<name>` handler, optional `_socket` fn, and `<Pascal>Request`
	// struct for axum) plus its `…/__server/<name>` route registration. A fn that
	// yields no body-bearing declaration is skipped (it can't be cleanly chunked).
	type FnRanges = { name: string; ranges: Range[] }
	const fns: FnRanges[] = []
	for (const name of names) {
		const ranges = declRanges(serverModule, name)
		if (ranges.length === 0) continue
		const route = routeRange(serverModule, name)
		if (route !== null) ranges.push(route)
		ranges.sort((a, b) => a.start - b.start)
		fns.push({ name, ranges })
	}
	if (fns.length === 0) return { preamble: serverModule, fns: [] }

	// The shared preamble is everything before the earliest declaration across all
	// fns (imports/`use`/app setup), shared by every chunk.
	const preambleEnd = Math.min(...fns.map((f) => f.ranges[0]!.start))
	const preamble = serverModule.slice(0, preambleEnd)

	const out = fns.map((f) => {
		// Concatenate this fn's pieces in source order, each on its own paragraph,
		// so the slice reads like a standalone module and contains only this fn's
		// body (a neighbouring fn's declaration is a separate range, never included).
		const slice = f.ranges
			.map((r) => trimTrailingBlankLines(serverModule.slice(r.start, r.end)))
			.filter((piece) => piece.length > 0)
			.join('\n\n')
		return { name: f.name, slice }
	})
	return { preamble, fns: out }
}

/** Strip a run of trailing blank lines, keeping a single terminating newline. */
function trimTrailingBlankLines(text: string): string {
	return text.replace(/\s+$/, '')
}

/**
 * Build the per-fn client binding shim: an import of the fn from its own chunk
 * (referenced by stable chunk id) plus a re-export under the author's name. This
 * is the only trace of the server fn that reaches the client bundle — the body
 * stays in the chunk `code`.
 */
function clientBindingFor(exportName: string, chunkId: string): string {
	const from = JSON.stringify(`./${chunkId}.server.js`)
	return `import { ${exportName} } from ${from};\nexport { ${exportName} };`
}

/**
 * Decompose a single back-compat `serverModule` blob into one
 * {@link ServerFnChunk} per exported server fn.
 *
 * `fileId` seeds each chunk's stable id (`<file>#<fnName>` hashed). When the
 * blob exposes one or more `…/__server/<name>` routes, each becomes its own
 * chunk whose `code` is `<shared preamble><fn slice>`. When no routes are
 * discoverable the whole blob is returned as a SINGLE chunk so nothing is lost.
 *
 * Returns an empty array only for an empty/whitespace blob.
 */
export function splitServerModule(
	fileId: string,
	serverModule: string
): ServerFnChunk[] {
	if (serverModule.trim().length === 0) return []

	const { preamble, fns } = partitionServerModule(serverModule)
	if (fns.length === 0) {
		// No per-fn routes found: keep the blob whole as one chunk so callers that
		// rely on chunking still get a usable unit (and the body never duplicates).
		const id = serverFnChunkId(fileId, '__server')
		return [
			{
				id,
				exportName: '__server',
				code: serverModule,
				clientBinding: clientBindingFor('__server', id),
			},
		]
	}

	const head = preamble.trim().length > 0 ? `${trimTrailingBlankLines(preamble)}\n\n` : ''
	const chunks: ServerFnChunk[] = []
	for (const fn of fns) {
		const id = serverFnChunkId(fileId, fn.name)
		const code = `${head}${fn.slice}`
		chunks.push({
			id,
			exportName: fn.name,
			code,
			clientBinding: clientBindingFor(fn.name, id),
		})
	}
	return chunks
}

/**
 * Build a server-fn manifest from a set of transform results: map each server
 * fn's stable {@link ServerFnChunk.id} to its export name and chunk reference,
 * so a bundler (or runtime) can resolve a fn id to the chunk that backs it.
 *
 * Accepts the transform results in any iterable; results with no `serverChunks`
 * contribute nothing. The chunk ref is the same stable id, which is the unit a
 * bundler emits the fn's chunk under.
 */
export function buildServerFnManifest(
	results: Iterable<TransformResult | null | undefined>
): ServerFnManifest {
	const manifest: ServerFnManifest = {}
	for (const result of results) {
		if (!result?.serverChunks) continue
		for (const chunk of result.serverChunks) {
			manifest[chunk.id] = { exportName: chunk.exportName, chunkRef: chunk.id }
		}
	}
	return manifest
}

/** Escape a string for safe interpolation into a `RegExp`. */
function escapeRegExp(input: string): string {
	return input.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}
