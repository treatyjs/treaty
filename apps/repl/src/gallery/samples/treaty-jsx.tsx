// Treaty JSX (.tsx) — a signals-by-default Angular component authored in JSX.
// NOT React: there is no `react` import. Props come from `input()`, state from
// `signal()`, the `.map()` lowers to `@for`, the ternary lowers to `@if/@else`,
// and the NAMED `onClick` handler becomes an Ivy `(click)` binding. The whole
// thing emits `ɵɵdefineComponent` (selectorless: class/selector derive from the
// file name).
import { input, signal } from '@angular/core'

export default function TaskList() {
	const heading = input('Tasks')
	const tasks = signal<readonly string[]>(['Design', 'Build', 'Ship'])

	const add = () => tasks.set([...tasks(), 'New task'])

	return (
		<section class="task-list">
			<h2>{heading()}</h2>
			{tasks().length
				? <ul>{tasks().map((task) => <li class="task">{task}</li>)}</ul>
				: <p class="empty">No tasks yet.</p>}
			<button onClick={add}>Add task</button>
		</section>
	)
}
