/**
 * Shared plain-TS types for the greeter feature.
 *
 * Types are ordinary TypeScript — they are erased at compile time and carry no
 * runtime cost, so a shared `*.types.ts` is the natural home for a shape used by
 * both the `.treaty` and `.tjsx` components. (Server functions, by contrast, are
 * declared INLINE in the component; a separate `*.server.ts` is optional.)
 */

export interface Greeting {
	readonly text: string
	readonly at: number
}
