/**
 * Application shell. Selectorless-by-default everywhere else in this app, the
 * ROOT is the one component that needs a stable element name so `index.html`'s
 * `<app-root>` can host it, so it declares `selector: 'app-root'` explicitly.
 *
 * It is a plain Angular `@Component` `.ts` — the third Treaty authoring surface —
 * lowered to Ivy by the `@treaty/vite` plugin like every other component. It
 * renders the top-level navigation across the eager index route and the four
 * lazy feature routes, plus the `<router-outlet>` the router fills.
 */
import { Component, signal } from '@angular/core'
import { RouterLink, RouterOutlet } from '@angular/router'

@Component({
	selector: 'app-root',
	imports: [RouterOutlet, RouterLink],
	template: `
		<header class="app-header">{{ title() }}</header>
		<nav class="app-nav">
			<a routerLink="/">logs</a>
			<a routerLink="/dashboard">dashboard</a>
			<a routerLink="/greeter">greeter</a>
			<a routerLink="/metrics">metrics</a>
			<a routerLink="/profile">profile</a>
		</nav>
		<main class="app-main">
			<router-outlet />
		</main>
	`,
})
export class AppRoot {
	/** Signal-by-default, like the rest of the app: the shell's heading text. */
	readonly title = signal('Treaty everything-app')
}
