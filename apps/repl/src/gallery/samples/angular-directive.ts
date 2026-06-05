// Angular @Directive (.ts) — attribute directive with host binding, listener, and input.
// Demonstrates idiomatic attribute directive with standalone: true, decorator-based host handling.
import { Directive, ElementRef, HostBinding, HostListener, Input, inject } from '@angular/core'

@Directive({
	selector: '[appHighlight]',
	standalone: true,
})
export class HighlightDirective {
	private readonly el = inject(ElementRef<HTMLElement>)

	/** Bindable color input — controls the highlight color (default: yellow). */
	@Input() appHighlight = 'yellow'

	/** Optional secondary color for active state (default: lightblue). */
	@Input() appHighlightActive = 'lightblue'

	/** Host binding — applies the current background color. */
	@HostBinding('style.backgroundColor') bgColor = this.appHighlight

	/** Host listener — change color on mouse enter (simulates activation). */
	@HostListener('mouseenter')
	onMouseEnter(): void {
		this.bgColor = this.appHighlightActive
	}

	/** Host listener — restore original color on mouse leave. */
	@HostListener('mouseleave')
	onMouseLeave(): void {
		this.bgColor = this.appHighlight
	}

	constructor() {
		// Add a subtle padding and transition for better UX
		this.el.nativeElement.style.padding = '0.5rem'
		this.el.nativeElement.style.transition = 'background-color 0.2s ease'
	}
}
