// Angular @Component (.ts) — standalone with signals, computed, @if/@for, and event handling.
// Demonstrates idiomatic Angular signals-based component with computed properties and control flow.
import { Component, computed, signal, ChangeDetectionStrategy } from '@angular/core'

@Component({
	selector: 'app-todo-item',
	standalone: true,
	changeDetection: ChangeDetectionStrategy.OnPush,
	imports: [],
	template: `
		<section class="todo-item">
			<h2>Todo Counter</h2>
			
			<div class="input-group">
				<input
					type="text"
					[value]="inputValue()"
					(input)="updateInput($event)"
					placeholder="Add a todo"
				/>
				<button type="button" (click)="addTodo()">Add</button>
			</div>

			@if (todos().length > 0) {
				<ul class="todo-list">
					@for (todo of todos(); track todo) {
						<li [class.completed]="todo.completed">
							<input
								type="checkbox"
								[checked]="todo.completed"
								(change)="toggleTodo(todo)"
							/>
							<span>{{ todo.text }}</span>
						</li>
					}
				</ul>
				<p class="status">{{ completed() }} of {{ todos().length }} completed</p>
			} @else {
				<p class="empty">No todos yet. Add one to get started!</p>
			}
		</section>
	`,
	styles: [`
		.todo-item {
			padding: 1rem;
			max-width: 400px;
			font-family: system-ui, sans-serif;
		}
		.input-group {
			display: flex;
			gap: 0.5rem;
			margin-bottom: 1rem;
		}
		input[type="text"] {
			flex: 1;
			padding: 0.5rem;
			border: 1px solid #ddd;
			border-radius: 4px;
		}
		button {
			padding: 0.5rem 1rem;
			background: #0066cc;
			color: white;
			border: none;
			border-radius: 4px;
			cursor: pointer;
		}
		button:hover {
			background: #0052a3;
		}
		.todo-list {
			list-style: none;
			padding: 0;
		}
		li {
			display: flex;
			gap: 0.5rem;
			padding: 0.5rem;
			border-bottom: 1px solid #eee;
		}
		li.completed span {
			text-decoration: line-through;
			color: #999;
		}
		.status, .empty {
			margin-top: 1rem;
			color: #666;
			font-size: 0.9rem;
		}
	`],
})
export class TodoItemComponent {
	readonly todos = signal<Array<{ text: string; completed: boolean }>>([])
	readonly inputValue = signal('')
	
	/** Computed: count of completed todos. */
	readonly completed = computed(() => 
		this.todos().filter(todo => todo.completed).length
	)

	addTodo(): void {
		const text = this.inputValue().trim()
		if (text) {
			this.todos.update(todos => [...todos, { text, completed: false }])
			this.inputValue.set('')
		}
	}

	toggleTodo(todo: { text: string; completed: boolean }): void {
		this.todos.update(todos =>
			todos.map(t => t === todo ? { ...t, completed: !t.completed } : t)
		)
	}

	updateInput(event: Event): void {
		const target = event.target as HTMLInputElement
		this.inputValue.set(target.value)
	}
}
