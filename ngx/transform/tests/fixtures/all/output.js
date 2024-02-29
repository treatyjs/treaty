import { Component } from "@angular/core";
import MyComponent from "../components/MyComponent.ngx"

@Component({
	selector: 'ngx',
	styles: [
		`
			h1 {
				background - color: var(--backgroundColor);
				color: var(--foregroundColor);
			}
		`
	],
	template: `
		<h1>{{someData}}</h1>
		<MyComponent class="red">This will be red!</MyComponent>
		<button (onClick)="doSomething()"> Do something</button>
	`
})
export class NGX {
	foregroundColor = "rgb(221 243 228)";
	backgroundColor = "rgb(24 121 78)";
	someData = "Hello World";
	constructor() {
		this.doSomething();
	}
	doSomething() {
		const a = 'abc';
		console.log(this.someData);
		console.log(a);
	}
}
