import { defineConfig } from 'vitest/config'

/**
 * Isolated vitest config for the affected-set tool. Self-contained (not part of
 * the root jest/nx project graph): runs the TypeScript sources directly in a Node
 * environment via vitest's built-in esbuild transform, so the suite needs no
 * build step and no main-workspace contention.
 */
export default defineConfig({
	test: {
		root: __dirname,
		environment: 'node',
		include: ['test/**/*.test.ts'],
		watch: false,
	},
})
