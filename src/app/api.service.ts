import { Injectable } from '@angular/core';
import { App } from 'server.ts';
import { edenClient } from './treaty/index.ts';

@Injectable({
  providedIn: 'root',
})
export class ApiService {
  client = edenClient<App>('http://localhost:4201').api;
}
