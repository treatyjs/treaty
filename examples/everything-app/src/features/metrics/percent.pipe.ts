/**
 * A PIPE (`.ts`, `@Pipe`) used in a template.
 *
 * Authored WITHOUT `standalone: true` -- the compiler fills that default in (a
 * pipe carries a real `name`, since that name is how the template references it,
 * but no standalone boilerplate). It is consumed by NAME in the metrics panel's
 * template (`{{ load() | percent01 }}`) and listed by CLASS in the host
 * component's `imports`. The Treaty compiler lowers this `@Pipe` source to a real
 * Ivy `ɵɵdefinePipe` (+ `ɵfac`) AOT -- no raw `@Pipe` decorator survives, so it
 * never falls to Angular's JIT at runtime -- and a host that imports it lists the
 * pipe class in its component `dependencies`, so `{{ … | percent01 }}` resolves.
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
