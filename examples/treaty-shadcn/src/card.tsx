/**
 * Card — a PLAIN REACT component compiled to Angular Ivy by Treaty.
 *
 * This is ordinary React: it imports `useState` from `react`, destructures its
 * props, and binds a NAMED handler to a button's `onClick`. Treaty's JSX
 * front-end lowers the React idioms to their Angular equivalents BEFORE its
 * signals-by-default pass runs:
 *
 *   - `import { useState } from 'react'`        → the `react` import is stripped
 *   - `const [expanded, setExpanded] = useState(false)` → `const expanded = signal(false)`
 *   - `setExpanded(v => !v)`                    → `expanded.update(v => !v)`
 *   - destructured props `{ title, description }` → signal `input()`s
 *   - `onClick={toggle}`                         → an Ivy `(click)` listener
 *
 * It proves the React → Angular path end to end: useState state, a named event
 * handler referenced bare in JSX, and props lowered to signal inputs.
 */
import { useState } from 'react'

export default function Card({ title, description }: { title: string; description?: string }) {
	const [expanded, setExpanded] = useState(false)

	// NAMED handler — referenced bare as `onClick={toggle}` (never an inline arrow).
	const toggle = () => setExpanded((v) => !v)

	return (
		<div className="card">
			<h3>{title}</h3>
			<button className="card-toggle" onClick={toggle}>
				{expanded ? 'Hide' : 'Show'} details
			</button>
			{expanded && <p className="card-description">{description}</p>}
		</div>
	)
}
