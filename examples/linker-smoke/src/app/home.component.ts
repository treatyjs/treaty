import {
  inject,
  signal,
  ɵɵdefineComponent,
  ɵɵtext,
  ɵɵtextInterpolate1,
  ɵɵadvance,
  ɵɵelementStart,
  ɵɵelementEnd,
  type ɵComponentDef,
} from '@angular/core';
import { PlatformLocation } from '@angular/common';

/**
 * The smoke component, authored in AOT Ivy form (a hand-written `ɵfac` + `ɵcmp`) with NO
 * `@Component` decorator, so it needs NO `@angular/compiler` and NEVER reaches the JIT fallback -
 * exactly the output an AOT compile (or the Treaty Rust component compiler) would emit.
 *
 * Why no decorator: `@treaty/ts-vite` lowers first-party files through SWC with `legacyDecorator`,
 * which PRESERVES the `@Component` decorator. A preserved decorator is evaluated at runtime and
 * queues JIT compilation (requiring `@angular/compiler`). Authoring the component as a plain class
 * carrying the Ivy `ɵfac`/`ɵcmp` static fields makes the runtime treat it as already-compiled - the
 * point under test is the LIBRARY linker, not first-party component compilation, so we keep the
 * first-party code AOT by construction.
 *
 * It injects `PlatformLocation` from `@angular/common` - the dependency whose partial-compiled
 * factory/injectable declarations triggered the original "needs JIT / `@angular/compiler` not
 * available" crash when the published `@angular/common` bundle was served un-linked. If the linker
 * did its job, `inject(PlatformLocation)` resolves a real `PlatformLocation` (the browser
 * `BrowserPlatformLocation`) with NO JIT fallback.
 */
export class HomeComponent {
  private readonly platformLocation = inject(PlatformLocation);

  /** True once a real `PlatformLocation` instance was resolved by DI. */
  readonly injected = signal(this.platformLocation instanceof PlatformLocation);

  /** The current pathname, read through the injected `PlatformLocation`. */
  readonly pathname = signal(this.platformLocation.pathname);

  static readonly ɵfac = function HomeComponent_Factory(): HomeComponent {
    return new HomeComponent();
  };

  // Hand-written Ivy component definition (AOT) - the same `ɵɵdefineComponent` shape the linker /
  // AOT compiler produces, so the runtime treats the class as already-compiled and never invokes
  // the JIT compiler.
  static readonly ɵcmp: ɵComponentDef<HomeComponent> = ɵɵdefineComponent({
    type: HomeComponent,
    selectors: [['smoke-home']],
    standalone: true,
    decls: 6,
    vars: 2,
    template: (rf: number, ctx: HomeComponent) => {
      if (rf & 1) {
        ɵɵelementStart(0, 'h1', 0);
        ɵɵtext(1, 'Linker smoke');
        ɵɵelementEnd();
        ɵɵelementStart(2, 'p', 1);
        ɵɵtext(3);
        ɵɵelementEnd();
        ɵɵelementStart(4, 'p', 2);
        ɵɵtext(5);
        ɵɵelementEnd();
      }
      if (rf & 2) {
        ɵɵadvance(3);
        ɵɵtextInterpolate1('pathname: ', ctx.pathname(), '');
        ɵɵadvance(2);
        ɵɵtextInterpolate1('PlatformLocation injected: ', ctx.injected(), '');
      }
    },
    consts: [
      ['id', 'smoke-heading'],
      ['id', 'smoke-pathname'],
      ['id', 'smoke-injected'],
    ],
    encapsulation: 2,
  });
}
