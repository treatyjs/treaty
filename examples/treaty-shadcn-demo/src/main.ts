/**
 * Browser bootstrap for the treaty-shadcn-demo.
 *
 * A plain `.ts` module (no `@Component`), so the `@treaty/vite` plugin passes it
 * straight through to Vite — only the authoring files it imports transitively
 * (the app-root `@Component` `.ts`) are lowered to Ivy. The six library
 * components it pulls in are ALREADY compiled Ivy (the built `treaty-shadcn/dist`
 * `.mjs`), so they ride through as plain ES modules.
 *
 * Standard standalone Angular bootstrap, signal-by-default + OnPush throughout:
 *   - `provideZonelessChangeDetection()` — no zone.js (the library + app are all
 *     signal/OnPush, mirroring the everything-app).
 */
import { provideZonelessChangeDetection } from '@angular/core'
import { bootstrapApplication } from '@angular/platform-browser'

// Global base theme (design tokens + the demo gallery layout). A plain `.css`
// side-effect import: Vite owns CSS natively and `@treaty/vite` does not claim
// `.css`. The library's component-scoped styles (Badge/Switch `<style>`) layer on
// top of this base, riding inside their lowered Ivy defs.
import './styles.css'

import { AppRoot } from './app/app-root.component'

void bootstrapApplication(AppRoot, {
	providers: [provideZonelessChangeDetection()],
})
