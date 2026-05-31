/**
 * Bootstrap root for the file-routed-app.
 *
 * The file-system route graph's top entry is `routes/layout.treaty` (the root
 * layout) at path `""`, loaded lazily like every other route. Angular still
 * needs ONE eager component to bootstrap into `<app-routed-root>` in index.html
 * and host the router; this is it. It declares an explicit `selector` (the only
 * component that needs a stable element name) and renders just the
 * `<router-outlet>` the generated routes fill — the root layout and all pages
 * render inside it.
 *
 * It is a plain Angular `@Component` `.ts`, lowered to Ivy by the `@treaty/vite`
 * plugin like every authoring file in the build.
 */
import { Component, signal } from '@angular/core'
import { RouterOutlet } from '@angular/router'

@Component({
	selector: 'app-routed-root',
	imports: [RouterOutlet],
	template: `<router-outlet />`,
})
export class RoutedRoot {
	/** Signal-by-default, like every route component: the app's display name. */
	readonly appName = signal('file-routed-app')
}
