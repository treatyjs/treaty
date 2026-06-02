import * as i0 from '@angular/core';
import { Component, Input } from '@angular/core';

class HelloComponent {
    name = 'World';
    static ɵfac = function HelloComponent_Factory(__ngFactoryType__) { return new (__ngFactoryType__ || HelloComponent)(); };
    static ɵcmp = /*@__PURE__*/ i0.ɵɵdefineComponent({ type: HelloComponent, selectors: [["tb-hello"]], inputs: { name: "name" }, decls: 2, vars: 1, consts: [[1, "greeting"]], template: function HelloComponent_Template(rf, ctx) { if (rf & 1) {
            i0.ɵɵdomElementStart(0, "h1", 0);
            i0.ɵɵtext(1);
            i0.ɵɵdomElementEnd();
        } if (rf & 2) {
            i0.ɵɵadvance();
            i0.ɵɵtextInterpolate1("Hello ", ctx.name);
        } }, encapsulation: 2 });
}
(() => { (typeof ngDevMode === "undefined" || ngDevMode) && i0.ɵsetClassMetadata(HelloComponent, [{
        type: Component,
        args: [{
                selector: 'tb-hello',
                standalone: true,
                template: '<h1 class="greeting">Hello {{ name }}</h1>',
            }]
    }], null, { name: [{
            type: Input
        }] }); })();
(() => { (typeof ngDevMode === "undefined" || ngDevMode) && i0.ɵsetClassDebugInfo(HelloComponent, { className: "HelloComponent", filePath: "hello.component.ts", lineNumber: 8 }); })();

class CounterComponent {
    count = 0;
    inc() {
        this.count++;
    }
    static ɵfac = function CounterComponent_Factory(__ngFactoryType__) { return new (__ngFactoryType__ || CounterComponent)(); };
    static ɵcmp = /*@__PURE__*/ i0.ɵɵdefineComponent({ type: CounterComponent, selectors: [["tb-counter"]], decls: 2, vars: 1, consts: [[3, "click"]], template: function CounterComponent_Template(rf, ctx) { if (rf & 1) {
            i0.ɵɵdomElementStart(0, "button", 0);
            i0.ɵɵdomListener("click", function CounterComponent_Template_button_click_0_listener() { return ctx.inc(); });
            i0.ɵɵtext(1);
            i0.ɵɵdomElementEnd();
        } if (rf & 2) {
            i0.ɵɵadvance();
            i0.ɵɵtextInterpolate1("count ", ctx.count);
        } }, encapsulation: 2 });
}
(() => { (typeof ngDevMode === "undefined" || ngDevMode) && i0.ɵsetClassMetadata(CounterComponent, [{
        type: Component,
        args: [{
                selector: 'tb-counter',
                standalone: true,
                template: '<button (click)="inc()">count {{ count }}</button>',
            }]
    }], null, null); })();
(() => { (typeof ngDevMode === "undefined" || ngDevMode) && i0.ɵsetClassDebugInfo(CounterComponent, { className: "CounterComponent", filePath: "counter.component.ts", lineNumber: 8 }); })();

/**
 * Generated bundle index. Do not edit.
 */

export { CounterComponent, HelloComponent };
//# sourceMappingURL=treaty-bench-widgets.mjs.map
