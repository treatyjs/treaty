/**
 * Test-only fixtures: a hand-rolled Elysia-shaped `App` type so the typing specs can assert
 * end-to-end inference of route response types without depending on a running Elysia instance.
 *
 * The shape mirrors what Elysia's compiler produces: an `App` exposing a `schema` keyed by route
 * path, then by HTTP method, with `{ body, headers, query, params, response }` per route and a
 * `response` keyed by status code.
 */

/** A user record returned by the fake `/users` GET route. */
export interface User {
  id: number
  name: string
  email: string
}

/** A post record returned by the fake `/posts/:id` GET route. */
export interface Post {
  id: number
  title: string
  authorId: number
}

/**
 * A fake Elysia application type. `EdenClient.Create<App>` only inspects `App['schema']`, so this is
 * sufficient to drive fully-typed client + resource inference in the specs.
 */
export interface FakeApp {
  schema: {
    '/users': {
      get: {
        body: unknown
        headers: unknown
        query: unknown
        params: unknown
        response: { 200: User[] }
      }
      post: {
        body: { name: string; email: string }
        headers: unknown
        query: unknown
        params: unknown
        response: { 200: User }
      }
    }
    '/posts/:id': {
      get: {
        body: unknown
        headers: unknown
        query: unknown
        params: unknown
        response: { 200: Post }
      }
    }
  }
}
