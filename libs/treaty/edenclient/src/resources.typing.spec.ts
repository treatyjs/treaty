/**
 * Compile-time typing specs. The assertions live in the types: if inference is wrong, `tsc`
 * (and jest's ts transform) fail to compile this file. A trivial runtime assertion keeps jest happy.
 *
 * Covers:
 *  - a typed client call infers the Elysia route's response type;
 *  - a typed resource (`edenHttpResource`) infers that same response type end-to-end;
 *  - the promise client + promise resource preserve typing.
 */

import type { Observable } from 'rxjs'
import type { ResourceRef } from '@angular/core'
import type { Client, PromiseClient } from './client'
import { edenHttpResource, edenPromiseResource } from './resources'
import type { EdenClient, InferResponse } from './lib/types'
import type { FakeApp, Post, User } from './testing/fixtures'
import type { Equal, Expect } from './testing/type-assert'

type AppClient = Client<FakeApp>

// --- Client call typing ----------------------------------------------------

// `client.users.get()` returns an ObservableResponse of the route's 200 type (User[]).
type UsersGetReturn = ReturnType<AppClient['users']['get']>
type _ClientUsersIsObservable = Expect<
  Equal<
    UsersGetReturn,
    EdenClient.ObservableResponse<User[]>
  >
>

// The detailed-response data field is exactly User[].
type UsersDetailed = UsersGetReturn extends Observable<infer D> ? D : never
type _ClientUsersData = Expect<
  Equal<UsersDetailed, EdenClient.DetailedResponse<User[]>>
>

// A parameterised route `posts/:id` resolves to the Post 200 response.
type PostsGetReturn = ReturnType<AppClient['posts'][string]['get']>
type _ClientPostsIsObservable = Expect<
  Equal<PostsGetReturn, EdenClient.ObservableResponse<Post>>
>

// `post` requires a typed body.
type UsersPostParam = Parameters<AppClient['users']['post']>[0]
type _ClientPostBody = Expect<Extends_<{ name: string; email: string }, UsersPostParam>>
type Extends_<A, B> = A extends B ? true : false

// --- InferResponse primitive ----------------------------------------------

type _Infer = Expect<Equal<InferResponse<UsersGetReturn>, User[]>>

// --- Resource response inference (end-to-end typesafe) ---------------------

declare const client: AppClient

// edenHttpResource infers Resource<User[] | undefined> from the route response.
const usersResource = edenHttpResource(() => client.users.get())
type _ResUsers = Expect<
  Equal<typeof usersResource, ResourceRef<User[] | undefined>>
>
type _ResUsersValue = Expect<
  Equal<ReturnType<typeof usersResource.value>, User[] | undefined>
>

// With a defaultValue, the undefined branch is removed.
const usersResourceDefaulted = edenHttpResource(() => client.users.get(), {
  defaultValue: [] as User[],
})
type _ResUsersDefault = Expect<
  Equal<ReturnType<typeof usersResourceDefaulted.value>, User[]>
>

// Parameterised resource infers Post.
const postResource = edenHttpResource(() => client.posts['1'].get())
type _ResPost = Expect<
  Equal<typeof postResource, ResourceRef<Post | undefined>>
>

// --- Promise client + promise resource -------------------------------------

declare const pclient: PromiseClient<AppClient>
type PUsersGet = ReturnType<pclientUsersGet>
type pclientUsersGet = PromiseClient<AppClient>['users']['get']
type _PromiseClientReturn = Expect<
  Equal<PUsersGet, Promise<EdenClient.DetailedResponse<User[]>>>
>

const pUsersResource = edenPromiseResource(() => pclient.users.get())
type _PResUsersValue = Expect<
  Equal<
    ReturnType<typeof pUsersResource.value>,
    EdenClient.DetailedResponse<User[]> | undefined
  >
>

describe('resources typing', () => {
  it('compiles the type-level assertions', () => {
    // The real assertions are the type aliases above; reaching here means they all held.
    expect(true).toBe(true)
  })
})
