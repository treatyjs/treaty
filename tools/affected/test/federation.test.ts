import { describe, expect, it } from 'vitest'

import {
	affectedFromInput,
	buildGraph,
	computeAffected,
	graphFromFederation,
	type FederatedModuleLike,
} from '../src/index.js'

/**
 * Mirrors what `@treaty/module-federation`'s `federatedModules(...)` returns: the
 * host, lazy routes-as-remotes, and libs — each with a stable moduleId, kind, and
 * backing path. The dependency EDGES are layered on separately (federation does
 * not model them).
 */
const FED_MODULES: FederatedModuleLike[] = [
	{ moduleId: 'shell', kind: 'host', path: 'remoteEntry.js' },
	{ moduleId: './routes/dashboard', kind: 'route', path: './src/app/dashboard' },
	{ moduleId: './routes/reports', kind: 'route', path: './src/app/reports' },
	{ moduleId: './libs/ui', kind: 'lib', path: './libs/ui' },
	{ moduleId: './libs/data-access', kind: 'lib', path: './libs/data-access' },
]

describe('graphFromFederation', () => {
	it('turns federatedModules + edges into a working affected graph', () => {
		const input = graphFromFederation(FED_MODULES, {
			dependencies: {
				'./routes/dashboard': ['./libs/ui', './libs/data-access'],
				'./routes/reports': ['./libs/ui'],
			},
		})

		// A shared-lib change fans out to its route dependents.
		const result = affectedFromInput(input, ['libs/ui/src/button.ts'])
		expect(result.directlyChangedIds).toEqual(['./libs/ui'])
		expect(result.affectedIds).toEqual([
			'./libs/ui',
			'./routes/dashboard',
			'./routes/reports',
		])

		// data-access only feeds dashboard.
		const r2 = affectedFromInput(input, ['libs/data-access/x.ts'])
		expect(r2.affectedIds).toEqual(['./libs/data-access', './routes/dashboard'])
	})

	it('produces independent modules when no edges are supplied', () => {
		const input = graphFromFederation(FED_MODULES)
		const result = affectedFromInput(input, ['libs/ui/x.ts'])
		// No edges => a change affects only the changed module.
		expect(result.affectedIds).toEqual(['./libs/ui'])
	})

	it('honours extraPaths (a module owning source beyond its backing path)', () => {
		const input = graphFromFederation(FED_MODULES, {
			extraPaths: { './routes/dashboard': ['./src/shared/dashboard-utils'] },
		})
		const graph = buildGraph(input)
		const result = computeAffected(graph, ['src/shared/dashboard-utils/format.ts'])
		expect(result.directlyChangedIds).toEqual(['./routes/dashboard'])
	})

	it('a bad edge from federation input fails loudly in buildGraph', () => {
		const input = graphFromFederation(FED_MODULES, {
			dependencies: { './routes/dashboard': ['./libs/ghost'] },
		})
		expect(() => buildGraph(input)).toThrow(/depends on unknown module: \.\/libs\/ghost/)
	})
})
