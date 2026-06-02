/**
 * treaty-shadcn-demo — the gallery application shell.
 *
 * A plain Angular `@Component` `.ts` (the third Treaty authoring surface), lowered
 * to Ivy by the `@treaty/vite` plugin like every component in the everything-app.
 * The ROOT is the one component that needs a stable element name so `index.html`'s
 * `<app-root>` can host it, so it declares `selector: 'app-root'` explicitly.
 *
 * It CONSUMES the built `treaty-shadcn` library: each of the six components is
 * imported by value from the package's per-component subpath export (the compiled
 * Ivy `.mjs` in `examples/treaty-shadcn/dist`, resolved through the workspace
 * alias in `vite.config.ts`) and listed in `imports: [...]`. They are referenced
 * SELECTORLESSLY in the template by their PascalCase tag — each lowered component
 * registers `selectors: [["button"], ["Button"]]` (the kebab + the capitalized
 * filename binding), so `<Button …>` matches the library's `Button` def at
 * runtime exactly the way the everything-app's greeter-page hosts `<Greeter />`.
 *
 * The gallery lays out every component across its full variant/size surface:
 *   - Button : all variants (default/outline/ghost/destructive) × all sizes (sm/md/lg)
 *   - Badge  : all variants (default/secondary/destructive/outline)
 *   - Card   : the REACT card, with its expand/collapse toggle
 *   - Alert  : the REACT alert, dismissible (× button) and non-dismissible
 *   - Input  : text / email / password / disabled, each with a placeholder + value
 *   - Switch : on / off / disabled, each with a label
 */
import { Component } from '@angular/core'

// The six components, imported by value from the built library. Each per-component
// subpath (`treaty-shadcn/button` …) is mapped to its compiled `dist/<name>/index.mjs`
// by the workspace alias in vite.config.ts; the default export is the lowered Ivy
// component (already AOT — no @Component decorator, no `react` import survives).
import Button from 'treaty-shadcn/button'
import Badge from 'treaty-shadcn/badge'
import Card from 'treaty-shadcn/card'
import Alert from 'treaty-shadcn/alert'
import Input from 'treaty-shadcn/input'
import Switch from 'treaty-shadcn/switch'

@Component({
	selector: 'app-root',
	// Children are referenced by VALUE (selectorless interop): the library defs are
	// listed here, and the template tags below match their registered selectors.
	imports: [Button, Badge, Card, Alert, Input, Switch],
	template: `
		<header class="demo-header">
			<h1>treaty-shadcn gallery</h1>
			<p>Six components — authored across .treaty, Treaty JSX, and plain React — consumed from the built library.</p>
		</header>

		<!-- BUTTON: every variant × every size (flat list of specs — one @for). -->
		<section class="demo-section" id="buttons">
			<h2>Button</h2>
			<div class="row">
				@for (btn of buttonSpecs; track btn.label) {
					<Button [variant]="btn.variant" [size]="btn.size" [label]="btn.label" />
				}
			</div>
			<div class="row">
				<span class="row-label">disabled</span>
				<Button [disabled]="true" label="disabled" />
			</div>
		</section>

		<!-- BADGE: every variant. -->
		<section class="demo-section" id="badges">
			<h2>Badge</h2>
			<div class="row">
				@for (variant of badgeVariants; track variant) {
					<Badge [variant]="variant" [label]="variant" />
				}
			</div>
		</section>

		<!-- CARD (REACT): expand/collapse toggle. -->
		<section class="demo-section" id="cards">
			<h2>Card <small>(React → Ivy)</small></h2>
			<div class="row">
				<Card title="Expandable card" description="Hidden detail revealed by the toggle button." />
				<Card title="Another card" description="Each card owns its own React useState → signal." />
			</div>
		</section>

		<!-- ALERT (REACT): dismissible + non-dismissible. -->
		<section class="demo-section" id="alerts">
			<h2>Alert <small>(React → Ivy)</small></h2>
			<div class="col">
				<Alert type="info" title="Heads up" message="This is an informational alert." />
				<Alert type="success" title="Saved" message="Your changes were saved successfully." />
				<Alert
					type="warning"
					title="Careful"
					message="This dismissible alert can be closed with the × button."
					[isDismissible]="true"
				/>
				<Alert
					type="error"
					title="Something broke"
					message="A dismissible error alert."
					[isDismissible]="true"
				/>
			</div>
		</section>

		<!-- INPUT: text / email / password / disabled. -->
		<section class="demo-section" id="inputs">
			<h2>Input</h2>
			<div class="col">
				<Input type="text" placeholder="Your name" value="Ada Lovelace" />
				<Input type="email" placeholder="you@example.com" value="ada@example.com" />
				<Input type="password" placeholder="••••••••" value="hunter2" />
				<Input type="text" placeholder="Disabled field" value="read only" [disabled]="true" />
			</div>
		</section>

		<!-- SWITCH: on / off / disabled, each labelled. -->
		<section class="demo-section" id="switches">
			<h2>Switch</h2>
			<div class="col">
				<Switch [checked]="false" label="Off by default" />
				<Switch [checked]="true" label="On by default" />
				<Switch [checked]="true" [disabled]="true" label="Disabled" />
			</div>
		</section>
	`,
})
export class AppRoot {
	/**
	 * The Button matrix as a FLAT spec list (variant × size precomputed), so the
	 * template needs only a single `@for`. The full variant/size surface is still
	 * exercised — every (variant, size) pair is one row entry.
	 */
	readonly buttonSpecs = (['default', 'outline', 'ghost', 'destructive'] as const).flatMap((variant) =>
		(['sm', 'md', 'lg'] as const).map((size) => ({ variant, size, label: `${variant} ${size}` })),
	)

	/** The Badge's four variants. */
	readonly badgeVariants = ['default', 'secondary', 'destructive', 'outline'] as const
}
