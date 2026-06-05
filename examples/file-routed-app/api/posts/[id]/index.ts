/**
 * Single post handler (api/posts/[id]/index.ts) -> endpoint "/posts/:id".
 *
 * The `[id]` directory is a DYNAMIC api segment, bound to the `id` parameter.
 * API paths always render dynamic segments in Angular's ":param" form, so this
 * endpoint is "/posts/:id" with the ordered param list ["id"].
 */
'use server'

import type { Post } from '../index'

const store: Post[] = [
	{ id: 1, slug: 'hello-world', title: 'Hello, world' },
	{ id: 2, slug: 'file-routing', title: 'File routing explained' },
]

export async function get(id: number): Promise<Post | undefined> {
	return store.find((p) => p.id === id)
}

export async function remove(id: number): Promise<boolean> {
	const index = store.findIndex((p) => p.id === id)
	if (index === -1) return false
	store.splice(index, 1)
	return true
}
