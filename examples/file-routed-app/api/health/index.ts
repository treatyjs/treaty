/**
 * Nested health-check handler (api/health/index.ts) -> endpoint "/health".
 *
 * A nested api directory: `health/` contributes the "health" segment and its
 * `index` handler is the endpoint. Demonstrates that api nesting works exactly
 * like routes nesting, one directory deep with no dynamic part.
 */
'use server'

export interface Health {
	readonly status: 'ok'
	readonly uptime: number
}

const startedAt = Date.now()

export async function handler(): Promise<Health> {
	return { status: 'ok', uptime: Date.now() - startedAt }
}
