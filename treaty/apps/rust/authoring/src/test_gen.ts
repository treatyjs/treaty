import {Injectable,inject,signal} from '@angular/core';
export class LoggerService4 {
	static ɵfac=function factory_LoggerService4(t) {
		return new (t || factory_LoggerService4)(i0.ɵɵinject(MY_SERVICE_TOKEN), i0.ɵɵinject(MY_SERVICE_TOKEN3), i0.ɵɵinject(MyService5));
	};

	constructor(myServiceToken, myServiceToken2, myServiceToken5){
		myServiceToken.lol();
	}
	logMessage(){
		console.log('Hello world!');
	}
}
import {TestService} from '../../test.service';
const test = inject(TestService);
test.sayHello();
const counter = signal(0);
function increase() {
	counter.update(v => v + 1);
}
