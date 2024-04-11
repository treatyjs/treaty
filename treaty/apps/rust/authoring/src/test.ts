import { Injectable, inject, signal, Component } from '@angular/core';

@Injectable()
export class LoggerService4 {
	constructor(@Inject(MY_SERVICE_TOKEN) myServiceToken: MyService, @Inject(MY_SERVICE_TOKEN3) myServiceToken2: MyService3, myServiceToken5: MyService5) {
		myServiceToken.lol();
	}
	logMessage() {
		console.log('Hello world!')
	}
}



@Component({
	selector: 'app-home',
	standalone: true,
	providers: [TestService],
})
export default class HomeComponent {

	constructor(private testService: LoggerService4) {
		testService.logMessage();
	}

	count = 0;

	increment() {
		this.count++;
	}
}

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