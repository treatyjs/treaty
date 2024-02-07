import { ɵprovideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideClientHydration } from '@angular/platform-browser';
import {
  BrowserDynamicTestingModule,
  platformBrowserDynamicTesting,
} from '@angular/platform-browser-dynamic/testing';
import { provideRouter, withComponentInputBinding } from '@angular/router';
import { beforeAll } from 'bun:test';
import { routes } from './app/app.routes';

beforeAll(async () => {
  TestBed.initTestEnvironment(
    [BrowserDynamicTestingModule],
    platformBrowserDynamicTesting()
  );

  TestBed.configureTestingModule({
    providers: [
      ɵprovideZonelessChangeDetection(),
      provideRouter(routes, withComponentInputBinding()),
      provideClientHydration(),
    ],
  });
});
