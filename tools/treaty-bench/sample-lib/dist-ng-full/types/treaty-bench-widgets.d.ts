import * as i0 from '@angular/core';

declare class HelloComponent {
    name: string;
    static ɵfac: i0.ɵɵFactoryDeclaration<HelloComponent, never>;
    static ɵcmp: i0.ɵɵComponentDeclaration<HelloComponent, "tb-hello", never, { "name": { "alias": "name"; "required": false; }; }, {}, never, never, true, never>;
}

declare class CounterComponent {
    count: number;
    inc(): void;
    static ɵfac: i0.ɵɵFactoryDeclaration<CounterComponent, never>;
    static ɵcmp: i0.ɵɵComponentDeclaration<CounterComponent, "tb-counter", never, {}, {}, never, never, true, never>;
}

export { CounterComponent, HelloComponent };
