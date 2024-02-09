import { GlobalRegistrator } from '@happy-dom/global-registrator';
import { afterEach, beforeEach } from 'bun:test';

GlobalRegistrator.register();

import { ɵprovideZonelessChangeDetection } from '@angular/core';
import { getTestBed } from '@angular/core/testing';
import {
  BrowserDynamicTestingModule,
  platformBrowserDynamicTesting,
} from '@angular/platform-browser-dynamic/testing';

const testBed = getTestBed();

testBed.initTestEnvironment(
  [BrowserDynamicTestingModule],
  platformBrowserDynamicTesting()
);

beforeEach(() => {
  testBed.configureTestingModule({
    providers: [ɵprovideZonelessChangeDetection()],
  });
});

afterEach(() => {
  testBed.resetTestingModule();
});
