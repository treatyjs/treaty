import { inject, signal } from '@angular/core';
import { TestService } from '../../test.service';
const test = inject(TestService);
test.sayHello()
	<style>
  h1 {
	background - color: black;
	color: white;
}
</style>
const counter = signal(0);
<h1>{{ counter() }} - {{ test.sayHello() }} </h1>


function increase() {
	counter.update(v => v + 1);
}

<button (click)="increase()" > Increase value </button>