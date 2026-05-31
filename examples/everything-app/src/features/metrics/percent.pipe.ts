/**
 * A PIPE (`.ts`, `@Pipe`) used in a template.
 *
 * Authored WITHOUT `standalone: true` -- the compiler fills that default in (a
 * pipe carries a real `name`, since that name is how the template references it,
 * but no standalone boilerplate). It is consumed by NAME in the `.treaty`
 * gauge's template (`{{ value() | percent01 }}`) and listed by CLASS in the
 * host component's `imports`. As with directives, a standalone `@Pipe` source is
 * a clean pass-through at the Treaty compiler stage (only components lower to
 * `ɵɵdefineComponent` here), so `verify.mjs` registers it as PASSTHROUGH; its
 * USAGE is exercised by the `.treaty` component that pipes through it.
 *
 * Behavior: formats a 0..1 ratio as a whole-number percentage (`0.4237` ->
 * `"42%"`), with an optional fraction-digits argument (`value | percent01:1`).
 */
import { Pipe, type PipeTransform } from '@angular/core'

@Pipe({ name: 'percent01' })
export class Percent01Pipe implements PipeTransform {
	transform(ratio: number, fractionDigits = 0): string {
		if (!Number.isFinite(ratio)) return '--%'
		const clamped = Math.min(1, Math.max(0, ratio))
		return `${(clamped * 100).toFixed(fractionDigits)}%`
	}
}
