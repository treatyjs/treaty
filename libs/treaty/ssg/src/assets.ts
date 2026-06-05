/**
 * @module
 *
 * **Static asset copying** for `@treaty/ssg`. A prerendered site usually ships
 * static assets (a `public/` or `assets/` dir of images, fonts, CSS, favicon)
 * alongside the generated HTML. This module recursively enumerates an asset
 * source directory and copies every file into the output directory, preserving
 * relative layout, and reports what it copied so the site generator can fold the
 * results into its artifact manifest.
 *
 * I/O is injectable (a {@link FileSystemPort}) so the copy is testable without
 * touching disk and a platform can redirect it; the default port uses
 * `node:fs/promises`. Treaty is a compiler, not a host — this only EMITS files.
 */

/** The filesystem operations asset copying needs, injectable for testing. */
export interface FileSystemPort {
	/** List a directory's entries, tagged file-vs-directory. Throws if absent. */
	readDir(dir: string): Promise<readonly { name: string; isDirectory: boolean }[]>
	/** Read a file's bytes. */
	readFile(path: string): Promise<Uint8Array>
	/** Write a file's bytes, creating parent directories as needed. */
	writeFile(path: string, contents: Uint8Array): Promise<void>
	/** Whether a path exists (used to no-op a missing asset dir). */
	exists(path: string): Promise<boolean>
}

/** One asset that was copied, for the artifact manifest. */
export interface CopiedAsset {
	/** Path relative to the asset source root (`'img/logo.png'`, forward slashes). */
	readonly path: string
	/** Absolute/joined output path the file was written to. */
	readonly output: string
	/** Byte length of the copied file. */
	readonly bytes: number
}

/** Join two path segments with a single forward slash. */
function joinPath(a: string, b: string): string {
	if (a === '') return b
	const left = a.replace(/[/\\]+$/, '')
	const right = b.replace(/^[/\\]+/, '')
	return `${left}/${right}`
}

/**
 * A {@link FileSystemPort} backed by `node:fs/promises`. Created lazily so the
 * package carries no static `node:*` import for callers that inject their own
 * port (or never copy assets at all).
 */
export async function createNodeFileSystem(): Promise<FileSystemPort> {
	const fs = await import('node:fs/promises')
	return {
		async readDir(dir) {
			const entries = await fs.readdir(dir, { withFileTypes: true })
			return entries.map((e) => ({ name: e.name, isDirectory: e.isDirectory() }))
		},
		async readFile(path) {
			const buf = await fs.readFile(path)
			return new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength)
		},
		async writeFile(path, contents) {
			const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'))
			if (slash > 0) await fs.mkdir(path.slice(0, slash), { recursive: true })
			await fs.writeFile(path, contents)
		},
		async exists(path) {
			try {
				await fs.access(path)
				return true
			} catch {
				return false
			}
		},
	}
}

/**
 * Recursively copy every file under `sourceDir` into `outDir`, preserving the
 * relative directory layout, and return the list of {@link CopiedAsset}s in
 * deterministic (lexicographically sorted, depth-first) order. A `sourceDir`
 * that does not exist is a no-op (returns `[]`) so an app without an asset
 * directory needs no special-casing.
 */
export async function copyAssets(
	sourceDir: string,
	outDir: string,
	fs: FileSystemPort
): Promise<CopiedAsset[]> {
	if (!(await fs.exists(sourceDir))) return []
	const copied: CopiedAsset[] = []
	await copyDir(sourceDir, '', outDir, fs, copied)
	return copied
}

/** Depth-first copy of one directory level, recursing into subdirectories. */
async function copyDir(
	sourceRoot: string,
	rel: string,
	outDir: string,
	fs: FileSystemPort,
	copied: CopiedAsset[]
): Promise<void> {
	const here = rel === '' ? sourceRoot : joinPath(sourceRoot, rel)
	const entries = [...(await fs.readDir(here))].sort((a, b) => a.name.localeCompare(b.name))
	for (const entry of entries) {
		const childRel = rel === '' ? entry.name : `${rel}/${entry.name}`
		if (entry.isDirectory) {
			await copyDir(sourceRoot, childRel, outDir, fs, copied)
			continue
		}
		const bytes = await fs.readFile(joinPath(sourceRoot, childRel))
		const output = joinPath(outDir, childRel)
		await fs.writeFile(output, bytes)
		copied.push({ path: childRel, output, bytes: bytes.byteLength })
	}
}
