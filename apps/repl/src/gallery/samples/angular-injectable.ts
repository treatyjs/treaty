// Angular @Injectable (.ts) — a standard root-provided service with constructor
// DI: a required dependency plus an @Optional() one. Lowers to `ɵfac` (the
// `ɵɵinject` factory, with InjectFlags 8 for the optional dep) + `ɵɵdefineInjectable`.
import { Injectable, Optional } from '@angular/core'
import { HttpClient } from '@angular/common/http'

@Injectable({ providedIn: 'root' })
export class ConsoleLogger {
	log(message: string): void {
		console.log('[todos]', message)
	}
}

@Injectable({ providedIn: 'root' })
export class TodoService {
	private readonly base = '/api/todos'

	constructor(
		private readonly http: HttpClient,
		@Optional() private readonly logger: ConsoleLogger | null,
	) {}

	list() {
		this.logger?.log(`GET ${this.base}`)
		return this.http.get<readonly string[]>(this.base)
	}

	add(title: string) {
		return this.http.post(this.base, { title })
	}
}
