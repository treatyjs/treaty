import '@angular/compiler'
import './tools/mock-zone';
import { bootstrapApplication } from '@angular/platform-browser';
import App from './app/app.treaty';
import { AppComponent } from './app/app.component';
import { appConfig } from './app/app.config';

bootstrapApplication(App, appConfig).catch((err) =>
	console.error(err)
);