/**
 * @module
 *
 * Implementation namespace backing the global `JSX` contract declared in
 * `./jsx-types.ts`. Splitting the machinery into the `TreatyJsx` namespace
 * keeps the public `JSX` surface a thin, readable set of aliases while this
 * file carries the element maps, the signal-aware value typing, the DOM event
 * surface, and the `use:` directive attributes.
 *
 * Everything here is ambient (`declare global`) and runtime-free.
 */

export {}

declare global {
	/**
	 * Treaty JSX building blocks. Authors never reference `TreatyJsx` directly;
	 * the global `JSX` namespace aliases into it.
	 */
	namespace TreatyJsx {
		// ---------------------------------------------------------------------
		// Signals
		// ---------------------------------------------------------------------

		/**
		 * A read-only signal: a zero-argument callable returning the current value.
		 * Compatible with Angular's `Signal<T>` and any `() => T` getter, so a
		 * `WritableSignal<T>` (which is also callable) is accepted.
		 */
		interface ReadableSignal<T> {
			(): T
		}

		/**
		 * A value usable in a binding position. Treaty unwraps signals at the
		 * binding site, so wherever a plain `T` is accepted a `{signal}` (or any
		 * `() => T` getter) is accepted too. The plain `T` arm preserves nullable
		 * bindings (`T` may itself include `undefined` / `null`).
		 */
		type Bindable<T> = T | ReadableSignal<T>

		/**
		 * Apply {@link Bindable} across every property of an attribute bag, so each
		 * attribute independently accepts its plain value or a bound signal of it.
		 */
		type Bindables<T> = {
			[K in keyof T]: Bindable<T[K]>
		}

		// ---------------------------------------------------------------------
		// Core JSX shapes
		// ---------------------------------------------------------------------

		/** Opaque handle for a compiled element/view node. */
		interface Element {
			readonly __treatyElement: unique symbol
		}

		/**
		 * Selectorless component shape: any callable/constructable value is a valid
		 * element class. The compiler owns instantiation, so neither the parameter
		 * list nor the result is constrained here.
		 */
		interface ElementClass {
			// Permissive on purpose: a function component or an Angular-style class
			// both satisfy "selectorless component value".
		}

		/** A single child node accepted in content position. */
		type Child =
			| Element
			| string
			| number
			| boolean
			| null
			| undefined
			| ReadableSignal<string | number | boolean | null | undefined>

		/** Children as authored: a single child or an (arbitrarily nested) array. */
		type Children = Child | ChildArray
		interface ChildArray extends ReadonlyArray<Children> {}

		// ---------------------------------------------------------------------
		// Directive authoring
		// ---------------------------------------------------------------------

		/**
		 * A single Angular host-binding/listener entry, as authored on a directive's
		 * host spec. The KEY is the binding microsyntax Angular understands
		 * (`'[style.color]'`, `'[attr.data-hl]'`, `'(click)'`, `'class.active'`, a
		 * plain attribute name, …) and the VALUE is the expression string evaluated
		 * against the directive instance. Keeping the value a string mirrors how the
		 * compiler lowers `host: { … }` to `ɵɵhostProperty` / `ɵɵlistener`.
		 */
		type HostBindings = Readonly<Record<string, string>>

		/**
		 * The object a directive-authoring function returns: the directive's host
		 * spec. Mirrors the `host` field of `@Directive({ host: … })`, so a function
		 * directive (`() => ({ host: { '[attr.data-hl]': 'on()' } })`) and a
		 * decorated class express the same host contract. Every field is optional so
		 * a bare side-effecting directive may return `{}`.
		 */
		interface HostSpec {
			/** Host bindings/listeners, keyed by Angular host microsyntax. */
			host?: HostBindings
		}

		/**
		 * A directive authored as a FUNCTION. The Treaty way: a plain function whose
		 * dependencies arrive as defaulted parameters (`el = inject(ElementRef)`),
		 * which runs its setup (effects, signals) and returns its {@link HostSpec}
		 * (or nothing, for a purely side-effecting directive). The compiler lowers
		 * the function to a selectorless Ivy `ɵɵdefineDirective`.
		 *
		 * `A` is the directive's accepted input — the value passed at the
		 * application site (`use:highlight={expr}`); defaults to `void` for an
		 * input-less directive applied bare (`use:highlight`).
		 */
		interface DirectiveFn<A = void> {
			(input?: A): HostSpec | void
		}

		/**
		 * A directive authored as a CLASS — any constructable value is accepted
		 * (the `@Directive`-decorated class form). Treaty is selectorless, so the
		 * instance shape is irrelevant to the application site; the compiler owns
		 * instantiation and host-binding wiring.
		 */
		interface DirectiveClass {
			new (...args: never[]): object
		}

		/**
		 * A value usable as a Treaty directive: the function form
		 * ({@link DirectiveFn}) or the decorated-class form ({@link DirectiveClass}).
		 * Authors annotate a directive with `Directive` (input-less) or
		 * `Directive<Input>` and get editor support for the returned host spec
		 * without hand-declaring anything.
		 */
		type Directive<A = void> = DirectiveFn<A> | DirectiveClass

		/**
		 * The input type a `use:<name>` application accepts for a given directive
		 * value `D`. A {@link DirectiveFn} surfaces its declared parameter; any
		 * other directive value (a class, or an untyped function) accepts `unknown`,
		 * so an application never over-constrains.
		 */
		type DirectiveInput<D> = D extends DirectiveFn<infer A> ? A : unknown

		// ---------------------------------------------------------------------
		// Pipe authoring
		// ---------------------------------------------------------------------

		/**
		 * A pipe authored as a TRANSFORM FUNCTION. The Treaty way: a plain function
		 * mapping an input value (plus any pipe arguments) to the transformed
		 * output, mirroring Angular `PipeTransform.transform`. The compiler lowers
		 * it to a selectorless Ivy `ɵɵdefinePipe`. Authors annotate with
		 * `Pipe<In, Out, Args>` to get parameter/return checking and completion.
		 *
		 *  - `In`   — the piped value's type (the left-hand side of `value | name`).
		 *  - `Out`  — the transformed result.
		 *  - `Args` — the trailing pipe arguments (`value | name:a:b`); a tuple.
		 */
		interface Pipe<In = unknown, Out = unknown, Args extends readonly unknown[] = readonly unknown[]> {
			(value: In, ...args: Args): Out
		}

		/**
		 * The Angular-shaped `PipeTransform` contract, for a pipe authored as a
		 * CLASS (`class X implements PipeTransform { transform(…) {} }`). Provided
		 * here so a class pipe type-checks against the same surface without pulling
		 * in `@angular/core` purely for the interface.
		 */
		interface PipeTransform {
			transform(value: unknown, ...args: readonly unknown[]): unknown
		}

		// ---------------------------------------------------------------------
		// Framework + directive attributes
		// ---------------------------------------------------------------------

		/**
		 * `use:<directive>` attributes. Treaty exposes structural/attribute
		 * directives through the `use:` namespace; the value is the directive's
		 * input (or `true` for a bare, input-less directive such as `use:autofocus`).
		 *
		 * The catch-all index keeps any `use:<name>` accepted while still typing the
		 * well-known built-ins. Bare directives accept `true | "" | string` so both
		 * `use:autofocus` and `use:autofocus={expr}` typecheck. The index value is
		 * intentionally open (`unknown`): a directive's concrete input is the
		 * directive function's own parameter (see {@link DirectiveInput}), which the
		 * compiler resolves by name; the attribute surface only guarantees the
		 * application site is accepted.
		 */
		interface UseDirectives {
			/** Focus the element once it is created. */
			'use:autofocus'?: Bindable<boolean | '' | string>
			/**
			 * Bind/merge classes imperatively. Complements the `class` attribute;
			 * accepts a string, a record of `{ name: condition }`, or a list.
			 */
			'use:class'?: Bindable<ClassValue>
			/** Bind inline styles imperatively. */
			'use:style'?: Bindable<StyleValue>
			/** Any other `use:<directive>` attribute. */
			[directive: `use:${string}`]: Bindable<unknown> | undefined
		}

		/**
		 * Framework-managed attributes Treaty understands on every element:
		 * `key` for list identity and `ref` for an element/instance handle.
		 */
		interface FrameworkAttributes {
			key?: Bindable<string | number>
			ref?: unknown
		}

		/** Names the children prop so completion offers `children`. */
		interface ChildrenAttribute {
			children?: Children
		}

		/** Attributes valid on every intrinsic element before its own attributes. */
		type IntrinsicAttributes = FrameworkAttributes & UseDirectives & ChildrenAttribute

		/**
		 * Component attribute typing: a component's own props (with each prop made
		 * signal-bindable) plus the framework + `use:` attributes. The `C` type
		 * parameter (the component value) is unused because Treaty is selectorless —
		 * props come from `P`, which the compiler infers.
		 */
		type LibraryManagedAttributes<_C, P> =
			& Bindables<P>
			& FrameworkAttributes
			& UseDirectives
			& ChildrenAttribute

		// ---------------------------------------------------------------------
		// class / style value shapes
		// ---------------------------------------------------------------------

		/**
		 * Accepted shapes for `class` (and `use:class`): a raw string, a
		 * `{ token: condition }` map, or an (arbitrarily nested) list of either.
		 */
		type ClassValue =
			| string
			| undefined
			| null
			| false
			| Record<string, boolean | undefined | null>
			| ReadonlyArray<ClassValue>

		/** Accepted shapes for inline `style`: a CSS string or a property map. */
		type StyleValue = string | Readonly<Record<string, string | number | null | undefined>>

		// ---------------------------------------------------------------------
		// DOM events
		// ---------------------------------------------------------------------

		/**
		 * An event handler in a Treaty binding. The handler receives the DOM event;
		 * `$event`-style template handlers compile down to this.
		 */
		type EventHandler<E extends Event> = (event: E) => void

		/**
		 * The standard DOM event surface, keyed by Treaty's `on<Event>` attribute
		 * names. Covers the events authors reach for; the open index on
		 * {@link GlobalAttributes} keeps any other `on*` handler accepted.
		 */
		interface DomEvents {
			onClick?: EventHandler<MouseEvent>
			onDblClick?: EventHandler<MouseEvent>
			onMouseDown?: EventHandler<MouseEvent>
			onMouseUp?: EventHandler<MouseEvent>
			onMouseEnter?: EventHandler<MouseEvent>
			onMouseLeave?: EventHandler<MouseEvent>
			onMouseMove?: EventHandler<MouseEvent>
			onMouseOver?: EventHandler<MouseEvent>
			onMouseOut?: EventHandler<MouseEvent>
			onContextMenu?: EventHandler<MouseEvent>
			onWheel?: EventHandler<WheelEvent>

			onKeyDown?: EventHandler<KeyboardEvent>
			onKeyUp?: EventHandler<KeyboardEvent>
			onKeyPress?: EventHandler<KeyboardEvent>

			onInput?: EventHandler<Event>
			onChange?: EventHandler<Event>
			onSubmit?: EventHandler<SubmitEvent>
			onReset?: EventHandler<Event>
			onFocus?: EventHandler<FocusEvent>
			onBlur?: EventHandler<FocusEvent>
			onFocusIn?: EventHandler<FocusEvent>
			onFocusOut?: EventHandler<FocusEvent>

			onPointerDown?: EventHandler<PointerEvent>
			onPointerUp?: EventHandler<PointerEvent>
			onPointerMove?: EventHandler<PointerEvent>
			onPointerEnter?: EventHandler<PointerEvent>
			onPointerLeave?: EventHandler<PointerEvent>
			onPointerCancel?: EventHandler<PointerEvent>

			onTouchStart?: EventHandler<TouchEvent>
			onTouchEnd?: EventHandler<TouchEvent>
			onTouchMove?: EventHandler<TouchEvent>
			onTouchCancel?: EventHandler<TouchEvent>

			onDrag?: EventHandler<DragEvent>
			onDragStart?: EventHandler<DragEvent>
			onDragEnd?: EventHandler<DragEvent>
			onDragEnter?: EventHandler<DragEvent>
			onDragLeave?: EventHandler<DragEvent>
			onDragOver?: EventHandler<DragEvent>
			onDrop?: EventHandler<DragEvent>

			onScroll?: EventHandler<Event>
			onLoad?: EventHandler<Event>
			onError?: EventHandler<Event>
			onAnimationEnd?: EventHandler<AnimationEvent>
			onTransitionEnd?: EventHandler<TransitionEvent>
		}

		// ---------------------------------------------------------------------
		// HTML attributes
		// ---------------------------------------------------------------------

		/**
		 * Attributes shared by every HTML element. `class` is the natural Treaty
		 * form; `className` is accepted as the JSX-familiar alias. Both are
		 * signal-bindable. The open `data-*` / `aria-*` / `[name: string]` indexes
		 * keep custom attributes and bindings accepted.
		 */
		interface GlobalAttributes extends DomEvents {
			/** Natural Treaty class form. Accepts string, map, or list. */
			class?: Bindable<ClassValue>
			/** JSX-familiar alias for {@link GlobalAttributes.class}. */
			className?: Bindable<ClassValue>
			style?: Bindable<StyleValue>

			id?: Bindable<string>
			title?: Bindable<string>
			lang?: Bindable<string>
			dir?: Bindable<'ltr' | 'rtl' | 'auto'>
			hidden?: Bindable<boolean>
			tabIndex?: Bindable<number>
			role?: Bindable<string>
			slot?: Bindable<string>
			draggable?: Bindable<boolean>
			contentEditable?: Bindable<boolean | 'true' | 'false' | 'plaintext-only' | 'inherit'>
			spellcheck?: Bindable<boolean>

			[dataAttr: `data-${string}`]: Bindable<unknown> | undefined
			[ariaAttr: `aria-${string}`]: Bindable<unknown> | undefined
		}

		/** Anchor (`<a>`) specific attributes. */
		interface AnchorAttributes extends GlobalAttributes {
			href?: Bindable<string>
			target?: Bindable<string>
			rel?: Bindable<string>
			download?: Bindable<string | boolean>
		}

		/** `<input>` specific attributes. */
		interface InputAttributes extends GlobalAttributes {
			type?: Bindable<string>
			value?: Bindable<string | number>
			checked?: Bindable<boolean>
			placeholder?: Bindable<string>
			disabled?: Bindable<boolean>
			readonly?: Bindable<boolean>
			required?: Bindable<boolean>
			name?: Bindable<string>
			min?: Bindable<string | number>
			max?: Bindable<string | number>
			step?: Bindable<string | number>
			autocomplete?: Bindable<string>
		}

		/** `<button>` specific attributes. */
		interface ButtonAttributes extends GlobalAttributes {
			type?: Bindable<'button' | 'submit' | 'reset'>
			disabled?: Bindable<boolean>
			name?: Bindable<string>
			value?: Bindable<string | number>
		}

		/** `<label>` specific attributes. */
		interface LabelAttributes extends GlobalAttributes {
			for?: Bindable<string>
			htmlFor?: Bindable<string>
		}

		/** `<img>` specific attributes. */
		interface ImgAttributes extends GlobalAttributes {
			src?: Bindable<string>
			alt?: Bindable<string>
			width?: Bindable<string | number>
			height?: Bindable<string | number>
			loading?: Bindable<'eager' | 'lazy'>
		}

		/** `<select>`/`<textarea>`/`<option>` form-control attributes. */
		interface FormControlAttributes extends GlobalAttributes {
			value?: Bindable<string | number>
			disabled?: Bindable<boolean>
			required?: Bindable<boolean>
			name?: Bindable<string>
			placeholder?: Bindable<string>
			selected?: Bindable<boolean>
		}

		/**
		 * The intrinsic (lowercase) element map. Each element carries the framework
		 * + `use:` + children attributes plus its own HTML attributes. Elements not
		 * listed individually fall back to {@link GlobalAttributes}; the open index
		 * keeps any custom/lowercase tag accepted.
		 */
		interface IntrinsicElements {
			a: WithCommon<AnchorAttributes>
			input: WithCommon<InputAttributes>
			button: WithCommon<ButtonAttributes>
			label: WithCommon<LabelAttributes>
			img: WithCommon<ImgAttributes>
			select: WithCommon<FormControlAttributes>
			textarea: WithCommon<FormControlAttributes>
			option: WithCommon<FormControlAttributes>

			div: WithCommon<GlobalAttributes>
			span: WithCommon<GlobalAttributes>
			p: WithCommon<GlobalAttributes>
			section: WithCommon<GlobalAttributes>
			article: WithCommon<GlobalAttributes>
			header: WithCommon<GlobalAttributes>
			footer: WithCommon<GlobalAttributes>
			main: WithCommon<GlobalAttributes>
			nav: WithCommon<GlobalAttributes>
			aside: WithCommon<GlobalAttributes>
			ul: WithCommon<GlobalAttributes>
			ol: WithCommon<GlobalAttributes>
			li: WithCommon<GlobalAttributes>
			h1: WithCommon<GlobalAttributes>
			h2: WithCommon<GlobalAttributes>
			h3: WithCommon<GlobalAttributes>
			h4: WithCommon<GlobalAttributes>
			h5: WithCommon<GlobalAttributes>
			h6: WithCommon<GlobalAttributes>
			form: WithCommon<GlobalAttributes>
			table: WithCommon<GlobalAttributes>
			thead: WithCommon<GlobalAttributes>
			tbody: WithCommon<GlobalAttributes>
			tr: WithCommon<GlobalAttributes>
			td: WithCommon<GlobalAttributes>
			th: WithCommon<GlobalAttributes>
			pre: WithCommon<GlobalAttributes>
			code: WithCommon<GlobalAttributes>
			strong: WithCommon<GlobalAttributes>
			em: WithCommon<GlobalAttributes>
			small: WithCommon<GlobalAttributes>
			br: WithCommon<GlobalAttributes>
			hr: WithCommon<GlobalAttributes>

			/** Any other lowercase / custom element. */
			[element: string]: WithCommon<GlobalAttributes>
		}

		/**
		 * Compose an element's own attributes with the framework, `use:` directive,
		 * and children attributes every element accepts.
		 */
		type WithCommon<A> = A & FrameworkAttributes & UseDirectives & ChildrenAttribute
	}
}
