/**
 * Stream-transport server functions.
 *
 * An `async function*` (async generator) exported from a `'use server'` module
 * is lowered by Treaty onto a streaming transport (SSE / chunked). Each `yield`
 * becomes one chunk pushed to the client; the client binding the compiler emits
 * is an async iterable / signal that updates per chunk rather than resolving
 * once. The body runs server-side and is stripped from the client bundle.
 *
 * Transport: Stream (server-push, many values over time).
 */
'use server'

export interface LogLine {
	readonly seq: number
	readonly level: 'info' | 'warn' | 'error'
	readonly message: string
}

/**
 * Tail a server log. Streams `count` lines, one yield per line. The client sees
 * an async iterable of {@link LogLine} it can `for await` over or bind into a
 * resource that re-renders on each chunk.
 */
export async function* streamLogs(count: number): AsyncGenerator<LogLine> {
	const levels: LogLine['level'][] = ['info', 'warn', 'error']
	for (let seq = 1; seq <= count; seq++) {
		// Simulated work between chunks; on the server this awaits real I/O.
		await Promise.resolve()
		yield {
			seq,
			level: levels[seq % levels.length]!,
			message: `log line ${seq}`,
		}
	}
}
