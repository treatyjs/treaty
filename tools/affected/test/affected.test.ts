import { describe, expect, it } from 'vitest'

import {
	affectedFromInput,
	buildGraph,
	computeAffected,
	GraphError,
	type ProjectGraphInput,
} from '../src/index.js'

/**
 * A realistic Treaty federation fixture graph:
 *
 *   host (shell)            owns src/app
 *     ├─ depends on lib:ui, lib:data-access, route:dashboard, route:reports
 *   route:dashboard         owns src/app/dashboard
 *     ├─ depends on lib:ui, lib:data-access
 *   route:reports           owns src/app/reports
 *     ├─ depends on lib:charts
 *   lib:charts              owns libs/charts
 *     ├─ depends on lib:ui
 *   lib:ui                  owns libs/ui            (the deep shared lib)
 *   lib:data-access         owns libs/data-access
 *   route:settings          owns src/app/settings   (isolated: no deps, no dependents)
 *
 * Reverse-dependency (dependents) edges this implies:
 *   lib:ui          -> route:dashboard, lib:charts, host   (and via charts: route:reports)
 *   lib:data-access -> route:dashboard, host
 *   lib:charts      -> route:reports
 *   route:dashboard -> host
 *   route:reports   -> host
 *   route:settings  -> (none)
 */
const FIXTURE: ProjectGraphInput = {
	nodes: [
		{
			id: 'host:shell',
			kind: 'host',
			paths: ['src/app'],
			dependsOn: ['lib:ui', 'lib:data-access', 'route:dashboard', 'route:reports'],
		},
		{
			id: 'route:dashboard',
			kind: 'route',
			paths: ['src/app/dashboard'],
			dependsOn: ['lib:ui', 'lib:data-access'],
		},
		{
			id: 'route:reports',
			kind: 'route',
			paths: ['src/app/reports'],
			dependsOn: ['lib:charts'],
		},
		{ id: 'route:settings', kind: 'route', paths: ['src/app/settings'] },
		{ id: 'lib:charts', kind: 'lib', paths: ['libs/charts'], dependsOn: ['lib:ui'] },
		{ id: 'lib:ui', kind: 'lib', paths: ['libs/ui'] },
		{ id: 'lib:data-access', kind: 'lib', paths: ['libs/data-access'] },
	],
}

describe('computeAffected — core scenarios', () => {
	const graph = buildGraph(FIXTURE)

	it('a leaf change affects only that module', () => {
		// route:settings has no dependents.
		const result = computeAffected(graph, ['src/app/settings/page.ts'])
		expect(result.affectedIds).toEqual(['route:settings'])
		expect(result.directlyChangedIds).toEqual(['route:settings'])
		expect(result.unmatchedFiles).toEqual([])
		expect(result.affected).toEqual([
			{ id: 'route:settings', kind: 'route', directlyChanged: true },
		])
	})

	it('a leaf lib change with one dependent affects the lib + that dependent only', () => {
		// lib:data-access is consumed by route:dashboard and host (not reports/charts).
		const result = computeAffected(graph, ['libs/data-access/src/api.ts'])
		expect(result.affectedIds).toEqual(['host:shell', 'lib:data-access', 'route:dashboard'])
		expect(result.directlyChangedIds).toEqual(['lib:data-access'])
	})

	it('a deep shared-lib change fans out to ALL transitive dependents', () => {
		// lib:ui is consumed directly by dashboard + charts + host, and transitively
		// by reports (via charts). Everything except route:settings is affected.
		const result = computeAffected(graph, ['libs/ui/src/button.ts'])
		expect(result.affectedIds).toEqual([
			'host:shell',
			'lib:charts',
			'lib:ui',
			'route:dashboard',
			'route:reports',
		])
		expect(result.directlyChangedIds).toEqual(['lib:ui'])
		// route:settings is the only module NOT reached.
		expect(result.affectedIds).not.toContain('route:settings')
		// Transitive vs direct flags are correct.
		const reports = result.affected.find((m) => m.id === 'route:reports')
		expect(reports).toEqual({ id: 'route:reports', kind: 'route', directlyChanged: false })
		const ui = result.affected.find((m) => m.id === 'lib:ui')
		expect(ui).toEqual({ id: 'lib:ui', kind: 'lib', directlyChanged: true })
	})

	it('an unrelated change owned by no module affects nothing', () => {
		const result = computeAffected(graph, ['README.md', 'docs/architecture.md'])
		expect(result.affectedIds).toEqual([])
		expect(result.directlyChangedIds).toEqual([])
		expect(result.unmatchedFiles).toEqual(['README.md', 'docs/architecture.md'])
	})

	it('combines multiple changed modules into one affected closure', () => {
		const result = computeAffected(graph, [
			'src/app/settings/x.ts', // leaf
			'libs/data-access/y.ts', // -> dashboard + host
		])
		expect(result.affectedIds).toEqual([
			'host:shell',
			'lib:data-access',
			'route:dashboard',
			'route:settings',
		])
		expect(result.directlyChangedIds).toEqual(['lib:data-access', 'route:settings'])
	})

	it('attributes a file to the LONGEST owning prefix (nearest module wins)', () => {
		// host owns src/app; dashboard owns src/app/dashboard. A file under
		// dashboard belongs to dashboard, not the host.
		const result = computeAffected(graph, ['src/app/dashboard/list.ts'])
		expect(result.directlyChangedIds).toEqual(['route:dashboard'])
		// dashboard's dependent is host (transitive), so:
		expect(result.affectedIds).toEqual(['host:shell', 'route:dashboard'])
	})

	it('attributes a file directly under the host (not any route) to the host', () => {
		const result = computeAffected(graph, ['src/app/main.ts'])
		expect(result.directlyChangedIds).toEqual(['host:shell'])
		// host has no dependents.
		expect(result.affectedIds).toEqual(['host:shell'])
	})

	it('does NOT match a sibling whose path is a string-prefix but not a path-prefix', () => {
		// libs/ui must not own libs/ui-kit/*.
		const g = buildGraph({
			nodes: [
				{ id: 'lib:ui', kind: 'lib', paths: ['libs/ui'] },
				{ id: 'lib:ui-kit', kind: 'lib', paths: ['libs/ui-kit'] },
			],
		})
		const result = computeAffected(g, ['libs/ui-kit/index.ts'])
		expect(result.affectedIds).toEqual(['lib:ui-kit'])
	})

	it('normalizes backslash and ./ in changed paths', () => {
		const result = computeAffected(graph, ['.\\libs\\ui\\src\\button.ts'])
		expect(result.directlyChangedIds).toEqual(['lib:ui'])
	})

	it('ignores blank / empty changed entries', () => {
		const result = computeAffected(graph, ['', '   ', 'libs/ui/x.ts'])
		expect(result.directlyChangedIds).toEqual(['lib:ui'])
	})
})

describe('computeAffected — global triggers', () => {
	const graph = buildGraph(FIXTURE)

	it('a global-trigger change affects every module', () => {
		const result = computeAffected(graph, ['pnpm-lock.yaml'], {
			globalTriggers: ['pnpm-lock.yaml', 'tsconfig.base.json'],
		})
		expect(result.affectedIds).toEqual([
			'host:shell',
			'lib:charts',
			'lib:data-access',
			'lib:ui',
			'route:dashboard',
			'route:reports',
			'route:settings',
		])
		// Every module is flagged as directly changed by the global trigger.
		expect(result.affected.every((m) => m.directlyChanged)).toBe(true)
		// The global file is consumed, not reported as unmatched.
		expect(result.unmatchedFiles).toEqual([])
	})

	it('without the global trigger declared, the same file affects nothing', () => {
		const result = computeAffected(graph, ['pnpm-lock.yaml'])
		expect(result.affectedIds).toEqual([])
		expect(result.unmatchedFiles).toEqual(['pnpm-lock.yaml'])
	})
})

describe('cycles and self-edges', () => {
	it('a dependency cycle terminates and yields the whole cycle', () => {
		// a <-> b mutually depend; a change to either affects both.
		const g = buildGraph({
			nodes: [
				{ id: 'a', kind: 'lib', paths: ['libs/a'], dependsOn: ['b'] },
				{ id: 'b', kind: 'lib', paths: ['libs/b'], dependsOn: ['a'] },
			],
		})
		const result = computeAffected(g, ['libs/a/x.ts'])
		expect(result.affectedIds).toEqual(['a', 'b'])
	})
})

describe('affectedFromInput', () => {
	it('builds + computes in one call', () => {
		const result = affectedFromInput(FIXTURE, ['libs/ui/x.ts'])
		expect(result.directlyChangedIds).toEqual(['lib:ui'])
	})
})

describe('buildGraph — validation', () => {
	it('rejects a node with no id', () => {
		expect(() =>
			buildGraph({ nodes: [{ id: '', kind: 'lib', paths: ['x'] } as never] })
		).toThrow(GraphError)
	})

	it('rejects duplicate ids', () => {
		expect(() =>
			buildGraph({
				nodes: [
					{ id: 'a', kind: 'lib', paths: ['x'] },
					{ id: 'a', kind: 'lib', paths: ['y'] },
				],
			})
		).toThrow(/duplicate node id: a/)
	})

	it('rejects an unknown dependency edge', () => {
		expect(() =>
			buildGraph({
				nodes: [{ id: 'a', kind: 'lib', paths: ['x'], dependsOn: ['ghost'] }],
			})
		).toThrow(/depends on unknown module: ghost/)
	})

	it('rejects a self-dependency', () => {
		expect(() =>
			buildGraph({ nodes: [{ id: 'a', kind: 'lib', paths: ['x'], dependsOn: ['a'] }] })
		).toThrow(/depends on itself/)
	})

	it('rejects a node with no owning paths', () => {
		expect(() => buildGraph({ nodes: [{ id: 'a', kind: 'lib', paths: [] }] })).toThrow(
			/must own at least one path/
		)
	})

	it('rejects an invalid kind', () => {
		expect(() =>
			buildGraph({ nodes: [{ id: 'a', kind: 'widget' as never, paths: ['x'] }] })
		).toThrow(/invalid kind/)
	})

	it('rejects a non-graph input', () => {
		expect(() => buildGraph({} as never)).toThrow(/must be an object with a `nodes` array/)
	})
})
