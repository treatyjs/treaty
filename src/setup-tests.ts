import { GlobalRegistrator } from '@happy-dom/global-registrator';

GlobalRegistrator.register();

import { TestBed } from '@angular/core/testing';
import {
  BrowserDynamicTestingModule,
  platformBrowserDynamicTesting,
} from '@angular/platform-browser-dynamic/testing';

TestBed.initTestEnvironment(
  [BrowserDynamicTestingModule],
  platformBrowserDynamicTesting()
);
// beforeAll(async () => {

//   // TestBed.configureTestingModule({
//   //   providers: [
//   //     ɵprovideZonelessChangeDetection(),
//   //     provideRouter(routes, withComponentInputBinding()),
//   //     provideClientHydration(),
//   //   ],
//   // });
// });
