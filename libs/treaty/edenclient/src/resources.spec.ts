/**
 * Runtime specs for the `resources` entry point: drives an `edenHttpResource` backed by a real
 * (test-controlled) Angular `HttpClient` and asserts the status transitions
 * (loading -> resolved, and loading -> error) plus that the resolved value matches the typed
 * response.
 *
 * Uses `provideHttpClientTesting` so no network is hit; `HttpTestingController` flushes responses.
 */

import { Injector, runInInjectionContext } from '@angular/core'
import { TestBed } from '@angular/core/testing'
import { provideHttpClient } from '@angular/common/http'
import {
	HttpTestingController,
	provideHttpClientTesting,
} from '@angular/common/http/testing'

import { createClient } from './client'
import { edenHttpResource } from './resources'
import type { FakeApp, User } from './testing/fixtures'

describe('edenHttpResource (runtime)', () => {
	let httpMock: HttpTestingController
	let injector: Injector

	const domain = 'http://localhost:3000'

	beforeEach(() => {
		TestBed.configureTestingModule({
			providers: [provideHttpClient(), provideHttpClientTesting()],
		})
		httpMock = TestBed.inject(HttpTestingController)
		injector = TestBed.inject(Injector)
	})

	afterEach(() => {
		httpMock.verify()
	})

	/** Flush Angular's reactive graph (effects + resource scheduling). */
	const flush = () => TestBed.tick()

	/** Flush reactive graph, then settle microtasks/async resource transitions. */
	const settle = async () => {
		flush()
		// Drain the microtask queue so rxResource's stream emission lands, then flush effects.
		await Promise.resolve()
		await Promise.resolve()
		flush()
	}

	const makeUsersResource = () =>
		runInInjectionContext(injector, () => {
			const client = createClient<FakeApp>(domain)
			return edenHttpResource(() => client.users.get(), { injector })
		})

	it('transitions loading -> resolved and exposes the typed value', async () => {
		const users = makeUsersResource()

		// Kick the resource so it issues its request.
		flush()
		expect(users.status()).toBe('loading')
		expect(users.isLoading()).toBe(true)
		expect(users.value()).toBeUndefined()

		const payload: User[] = [
			{ id: 1, name: 'Ada', email: 'ada@example.com' },
		]
		httpMock.expectOne(`${domain}/users`).flush(payload)
		await settle()

		expect(users.status()).toBe('resolved')
		expect(users.isLoading()).toBe(false)
		expect(users.hasValue()).toBe(true)
		expect(users.value()).toEqual(payload)
		expect(users.error()).toBeUndefined()
	})

	it('transitions loading -> error when the request fails', async () => {
		const users = makeUsersResource()

		flush()
		expect(users.status()).toBe('loading')

		httpMock
			.expectOne(`${domain}/users`)
			.flush('boom', { status: 500, statusText: 'Server Error' })
		await settle()

		expect(users.status()).toBe('error')
		expect(users.error()).toBeInstanceOf(Error)
		expect(users.isLoading()).toBe(false)
	})

	it('reload() re-issues the request', async () => {
		const users = makeUsersResource()

		flush()
		httpMock
			.expectOne(`${domain}/users`)
			.flush([{ id: 1, name: 'Ada', email: 'ada@example.com' }] as User[])
		await settle()
		expect(users.status()).toBe('resolved')

		users.reload()
		flush()
		expect(users.status()).toBe('reloading')

		httpMock
			.expectOne(`${domain}/users`)
			.flush([
				{ id: 2, name: 'Grace', email: 'grace@example.com' },
			] as User[])
		await settle()

		expect(users.status()).toBe('resolved')
		expect(users.value()).toEqual([
			{ id: 2, name: 'Grace', email: 'grace@example.com' },
		])
	})
})
