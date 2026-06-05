/**
 * Root API handler (api/index.ts) -> endpoint "/".
 *
 * Files under `api/` are SERVER FUNCTIONS, not components. The `'use server'`
 * prologue marks the module server-only: Treaty extracts each exported handler
 * into the backend (axum by default) and the manifest records the endpoint. An
 * `index` handler contributes no extra URL segment, so this maps to "/". With
 * no method hint in the file name, it answers all HTTP methods.
 */
'use server'

export interface ApiInfo {
	readonly name: string
	readonly version: string
}

export async function handler(): Promise<ApiInfo> {
	return { name: 'file-routed-app', version: '1.0.0' }
}
