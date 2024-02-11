import { bootstrapApplication } from '@angular/platform-browser';
import { AppComponent } from './app/app.component';
import { appConfig } from './app/app.config';

type Zone = {
  current: {
    get(key: string): boolean | undefined;
    run<T>(fn: (...args: any[]) => T, applyThis?: any, applyArgs?: any): T;
  };
  run<T>(fn: (...args: any[]) => T, applyThis?: any, applyArgs?: any): T;
};
declare var Zone: Zone;
declare module globalThis {
  let Zone: Zone;
}

if (typeof Zone === 'undefined') {
  const mockZone = {
    current: {
      get: (key: string): boolean | undefined =>
        key === 'isAngularZone' ? true : undefined,
      run<T>(fn: (...args: any[]) => T, applyThis?: any, applyArgs?: any): T {
        return fn.apply(applyThis, applyArgs);
      },
    },
    run<T>(fn: (...args: any[]) => T, applyThis?: any, applyArgs?: any): T {
      return fn.apply(applyThis, applyArgs);
    },
  };
  globalThis.Zone = mockZone;
}

bootstrapApplication(AppComponent, appConfig).catch((err) =>
  console.error(err)
);
