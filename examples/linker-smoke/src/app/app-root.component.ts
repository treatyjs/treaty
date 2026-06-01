import {
  ɵɵdefineComponent,
  ɵɵelement,
  type ɵComponentDef,
} from '@angular/core';
import { RouterOutlet } from '@angular/router';

/**
 * Root shell, authored in AOT Ivy form (no `@Component` decorator - see HomeComponent for why). It
 * hosts the router outlet so the routed {@link HomeComponent} - which injects `PlatformLocation` -
 * renders through a real `provideRouter` configuration. `RouterOutlet` comes from `@angular/router`
 * (referenced in the component def `dependencies`), exercising the published router package too.
 */
// AOT Ivy component: the static ɵfac/ɵcmp fields ARE the component definition Angular's runtime
// consumes; the class is intentionally instance-less.
// oxlint-disable-next-line no-extraneous-class
export class AppRoot {
  static readonly ɵfac = function AppRoot_Factory(): AppRoot {
    return new AppRoot();
  };

  static readonly ɵcmp: ɵComponentDef<AppRoot> = ɵɵdefineComponent({
    type: AppRoot,
    selectors: [['smoke-root']],
    standalone: true,
    decls: 1,
    vars: 0,
    template: (rf: number) => {
      if (rf & 1) {
        ɵɵelement(0, 'router-outlet');
      }
    },
    dependencies: [RouterOutlet],
    encapsulation: 2,
  });
}
