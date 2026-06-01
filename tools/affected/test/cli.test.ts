import { mkdtempSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { afterAll, beforeAll, describe, expect, it } from 'vitest'

import { parseArgs, run, type CliIo } from '../src/index.js'

const GRAPH = {
	nodes: [
		{
			id: 'host:shell',
			kind: 'host',
			paths: ['src/app'],
			dependsOn: ['lib:ui', 'route:dashboard'],
		},
		{
			id: 'route:dashboard',
			kind: 'route',
			paths: ['src/app/dashboard'],
			dependsOn: ['lib:ui'],
		},
		{ id: 'route:settings', kind: 'route', paths: ['src/app/settings'] },
		{ id: 'lib:ui', kind: 'lib', paths: ['libs/ui'] },
	],
}

/** Build a CliIo over fixed argv + stdin, capturing stdout/stderr. */
function io(argv: string[], stdin = ''): CliIo & { out: string; err: string } {
	const cap = {
		out: '',
		err: '',
		argv,
		readStdin: async () => stdin,
		stdout(text: string) {
			cap.out += text
		},
		stderr(text: string) {
			cap.err += text
		},
	}
	return cap
}

describe('parseArgs', () => {
	it('parses all flags', () => {
		const opts = parseArgs([
			'--graph',
			'g.json',
			'--files',
			'f.txt',
			'--base',
			'origin/main...HEAD',
			'--global',
			'pnpm-lock.yaml',
			'--global',
			'tsconfig.base.json',
			'--format',
			'ids',
			'--fail-on-empty',
		])
		expect(opts.graph).toBe('g.json')
		expect(opts.files).toBe('f.txt')
		expect(opts.base).toBe('origin/main...HEAD')
		expect(opts.globalTriggers).toEqual(['pnpm-lock.yaml', 'tsconfig.base.json'])
		expect(opts.format).toBe('ids')
		expect(opts.failOnEmpty).toBe(true)
	})

	it('rejects an unknown flag', () => {
		expect(() => parseArgs(['--nope'])).toThrow(/unknown argument: --nope/)
	})

	it('rejects a flag missing its value', () => {
		expect(() => parseArgs(['--graph'])).toThrow(/--graph requires a value/)
	})

	it('rejects an invalid format', () => {
		expect(() => parseArgs(['--format', 'yaml'])).toThrow(/invalid --format: yaml/)
	})
})

describe('run — end to end via stdin', () => {
	it('reads graph from a file and changed files from stdin (json)', async () => {
		const tmp = mkdtempSync(join(tmpdir(), 'affected-cli-'))
		const graphPath = join(tmp, 'graph.json')
		writeFileSync(graphPath, JSON.stringify(GRAPH))
		try {
			const cap = io(['--graph', graphPath, '--files', '-'], 'libs/ui/button.ts\n')
			const code = await run(cap)
			expect(code).toBe(0)
			const parsed = JSON.parse(cap.out)
			expect(parsed.affectedIds).toEqual(['host:shell', 'lib:ui', 'route:dashboard'])
			expect(parsed.directlyChangedIds).toEqual(['lib:ui'])
			expect(parsed.count).toBe(3)
		} finally {
			rmSync(tmp, { recursive: true, force: true })
		}
	})

	it('a missing --files path errors cleanly (exit 1, no uncaught throw)', async () => {
		const cap = io(['--graph', '-', '--files', 'does/not/exist.txt'], JSON.stringify(GRAPH))
		const code = await run(cap)
		expect(code).toBe(1)
		expect(cap.err).toMatch(/cannot read does\/not\/exist\.txt/)
		expect(cap.err).toMatch(/ENOENT|no such file|cannot find/i)
	})

	it('ids format emits one affected id per line', async () => {
		const tmp = mkdtempSync(join(tmpdir(), 'affected-cli-'))
		const graphPath = join(tmp, 'graph.json')
		const filesPath = join(tmp, 'changed.txt')
		writeFileSync(graphPath, JSON.stringify(GRAPH))
		writeFileSync(filesPath, 'libs/ui/button.ts\n')
		try {
			const cap = io([
				'--graph',
				graphPath,
				'--files',
				filesPath,
				'--format',
				'ids',
			])
			const code = await run(cap)
			expect(code).toBe(0)
			expect(cap.out).toBe('host:shell\nlib:ui\nroute:dashboard\n')
		} finally {
			rmSync(tmp, { recursive: true, force: true })
		}
	})

	it('--fail-on-empty exits 1 when nothing is affected', async () => {
		const tmp = mkdtempSync(join(tmpdir(), 'affected-cli-'))
		const graphPath = join(tmp, 'graph.json')
		const filesPath = join(tmp, 'changed.txt')
		writeFileSync(graphPath, JSON.stringify(GRAPH))
		writeFileSync(filesPath, 'README.md\n')
		try {
			const cap = io([
				'--graph',
				graphPath,
				'--files',
				filesPath,
				'--format',
				'ids',
				'--fail-on-empty',
			])
			const code = await run(cap)
			expect(code).toBe(1)
			expect(cap.out).toBe('') // nothing affected
		} finally {
			rmSync(tmp, { recursive: true, force: true })
		}
	})

	it('seeds the graph from --federation JSON', async () => {
		const tmp = mkdtempSync(join(tmpdir(), 'affected-cli-'))
		const fedPath = join(tmp, 'fed.json')
		const filesPath = join(tmp, 'changed.txt')
		writeFileSync(
			fedPath,
			JSON.stringify({
				modules: [
					{ moduleId: 'shell', kind: 'host', path: 'remoteEntry.js' },
					{ moduleId: './routes/dashboard', kind: 'route', path: './src/app/dashboard' },
					{ moduleId: './libs/ui', kind: 'lib', path: './libs/ui' },
				],
				dependencies: { './routes/dashboard': ['./libs/ui'] },
			})
		)
		writeFileSync(filesPath, 'libs/ui/x.ts\n')
		try {
			const cap = io([
				'--federation',
				fedPath,
				'--files',
				filesPath,
				'--format',
				'ids',
			])
			const code = await run(cap)
			expect(code).toBe(0)
			expect(cap.out).toBe('./libs/ui\n./routes/dashboard\n')
		} finally {
			rmSync(tmp, { recursive: true, force: true })
		}
	})

	it('errors when neither --graph nor --federation is given', async () => {
		const cap = io(['--files', '-'], '')
		const code = await run(cap)
		expect(code).toBe(1)
		expect(cap.err).toMatch(/a project graph is required/)
	})

	it('errors on a malformed graph', async () => {
		const tmp = mkdtempSync(join(tmpdir(), 'affected-cli-'))
		const graphPath = join(tmp, 'graph.json')
		writeFileSync(graphPath, JSON.stringify({ nodes: [{ id: 'a', kind: 'lib', paths: ['x'], dependsOn: ['ghost'] }] }))
		try {
			const cap = io(['--graph', graphPath, '--files', '-'], 'x/y.ts')
			const code = await run(cap)
			expect(code).toBe(1)
			expect(cap.err).toMatch(/depends on unknown module: ghost/)
		} finally {
			rmSync(tmp, { recursive: true, force: true })
		}
	})

	it('prints help and exits 0', async () => {
		const cap = io(['--help'])
		const code = await run(cap)
		expect(code).toBe(0)
		expect(cap.out).toMatch(/treaty-affected/)
	})
})

// Sanity that the temp dir helper does not leak between cases.
describe('temp isolation', () => {
	let dir: string
	beforeAll(() => {
		dir = mkdtempSync(join(tmpdir(), 'affected-iso-'))
	})
	afterAll(() => {
		rmSync(dir, { recursive: true, force: true })
	})
	it('uses a fresh temp dir', () => {
		expect(dir).toContain('affected-iso-')
	})
})
