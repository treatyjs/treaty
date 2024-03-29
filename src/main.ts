// import '../treaty-utilities/mock-zone.ts';

import { bootstrapApplication } from '@angular/platform-browser';
import { AppComponent } from './app/app.component.ts';
import { appConfig } from './app/app.config.ts';

bootstrapApplication(AppComponent, appConfig).catch((err) =>
  console.error(err)
);
