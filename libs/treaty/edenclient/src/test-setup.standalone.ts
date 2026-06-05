// Standalone setup for the bypass jest config. Uses the v15 jest-preset-angular API
// (`setupZoneTestEnv` from `setup-env/zone`) since this environment ships jest-preset-angular 15.x.
import { setupZoneTestEnv } from 'jest-preset-angular/setup-env/zone';

setupZoneTestEnv({
  errorOnUnknownElements: true,
  errorOnUnknownProperties: true,
});
