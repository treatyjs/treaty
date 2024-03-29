import { bootstrapApplication } from '@angular/platform-browser';
import { AppComponent } from './app/app.component.ts';
import { config } from './app/app.config.server.ts';

const bootstrap = () => bootstrapApplication(AppComponent, config);

export default bootstrap;
