import {
  APP_INITIALIZER,
  ApplicationConfig,
  provideZonelessChangeDetection,
} from '@angular/core';

import { provideHttpClient, withFetch } from '@angular/common/http';
import { initHightlighter } from './utils/highlighter';

export const appConfig: ApplicationConfig = {
  providers: [
    provideZonelessChangeDetection(),
    provideHttpClient(withFetch()),
    {
      provide: APP_INITIALIZER,
      useValue: async () => await initHightlighter(),
      multi: true,
    },
  ],
};

