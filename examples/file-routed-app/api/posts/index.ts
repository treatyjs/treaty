/**
 * Posts collection handler (api/posts/index.ts) -> endpoint "/posts".
 *
 * The directory `posts/` contributes the "posts" path segment; this `index`
 * handler is the collection endpoint. No method hint in the name, so it answers
 * all methods (the backend plugin narrows to e.g. GET list / POST create).
 */
'use server'

export interface Post {
	readonly id: number
	readonly slug: string
	readonly title: string
}

const store: Post[] = [
	{ id: 1, slug: 'hello-world', title: 'Hello, world' },
	{ id: 2, slug: 'file-routing', title: 'File routing explained' },
]

export async function list(): Promise<Post[]> {
	return store.slice()
}

export async function create(title: string, slug: string): Promise<Post> {
	const created: Post = { id: store.length + 1, slug, title }
	store.push(created)
	return created
}
