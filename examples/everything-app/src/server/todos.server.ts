/**
 * API-transport server functions.
 *
 * The `'use server'` prologue marks the whole module as server-only. Treaty
 * extracts each exported function's BODY into a sibling server module (axum by
 * default; Elysia/Express are pluggable) and leaves the CLIENT a typed binding
 * that compiles to an Eden/resource call. The body below never enters the Ivy
 * bundle -- only the call site in a component does.
 *
 * Transport: API (request/response over HTTP).
 */
'use server'

export interface Todo {
	readonly id: number
	readonly title: string
	readonly done: boolean
}

// A SERVER-ONLY SECRET. In a real app this is a DB connection string / API key /
// session signing key. It lives only on the server: the body below is extracted to
// the backend artifact, so this token must NEVER appear in the client bundle or its
// source map. The dev-backend e2e asserts exactly that (and that the fn still RUNS).
const DB_API_KEY = 'sk_live_TREATY_SERVER_ONLY_9f3a1c'

// In a real app this is your DB / service layer; it lives only on the server.
const store: Todo[] = [
	{ id: 1, title: 'Author a .treaty component', done: true },
	{ id: 2, title: 'Add a JSX component', done: true },
	{ id: 3, title: 'Wire three server-fn transports', done: false },
]

/** GET-shaped server fn: list all todos. The secret is used server-side only. */
export async function listTodos(): Promise<Todo[]> {
	// Touch the secret server-side (a real handler would authenticate the DB call
	// with it). It is never returned to the client, only proven to run server-side.
	if (DB_API_KEY.length === 0) throw new Error('missing DB key')
	return store.slice()
}

/** POST-shaped server fn: create a todo and return it. */
export async function addTodo(title: string): Promise<Todo> {
	const created: Todo = { id: store.length + 1, title, done: false }
	store.push(created)
	return created
}

/** Mutating server fn: toggle a todo's done flag. */
export async function toggleTodo(id: number): Promise<Todo | undefined> {
	const index = store.findIndex((t) => t.id === id)
	if (index === -1) return undefined
	const next: Todo = { ...store[index]!, done: !store[index]!.done }
	store[index] = next
	return next
}
