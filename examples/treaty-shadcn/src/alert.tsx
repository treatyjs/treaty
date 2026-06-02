/**
 * `alert.tsx` -- a PLAIN REACT component (it imports the `useState` hook from
 * `react`) lowered straight to an Angular Ivy component by Treaty's JSX
 * front-end. NOT React at runtime: the `react` import is stripped, `useState`
 * becomes a `signal`, and the named `dismiss` handler's `setDismissed(true)`
 * lowers to `dismissed.set(true)` -- the whole thing emits `ɵɵdefineComponent`.
 *
 * It exercises the React -> Angular path end to end:
 *   - `useState` state + its setter        -> `signal` + `.set(...)`
 *   - destructured props (with defaults)    -> signal `input()`s
 *   - a NAMED `onClick` handler             -> an Ivy `(click)` binding
 *   - a `{!dismissed && <div/>}` conditional -> an Ivy `@if` block
 *
 * Selectorless: the class/selector derive from the file name (`Alert` / `alert`).
 */
import { useState } from 'react'

type AlertType = 'info' | 'warning' | 'error' | 'success'

export default function Alert({
	type = 'info' as AlertType,
	title,
	message,
	isDismissible = false,
}: {
	type?: AlertType
	title: string
	message: string
	isDismissible?: boolean
}) {
	// React state: lowered to `const dismissed = signal(false)`.
	const [dismissed, setDismissed] = useState(false)

	// NAMED handler (no inline arrow in JSX -- that path is a known gap). The
	// `setDismissed(true)` call lowers to `dismissed.set(true)`.
	const dismiss = () => setDismissed(true)

	// `{!dismissed && <div/>}` lowers to `@if (!dismissed) { <div/> }`, so the
	// whole alert disappears once dismissed.
	return (
		<>
			{!dismissed && (
				<div className={`alert alert-${type}`} role="alert">
					<div className="alert-body">
						<strong className="alert-title">{title}</strong>
						<p className="alert-message">{message}</p>
					</div>
					{isDismissible && (
						<button className="alert-dismiss" type="button" aria-label="Dismiss" onClick={dismiss}>
							×
						</button>
					)}
				</div>
			)}
		</>
	)
}
