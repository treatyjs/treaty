import { Injectable } from '@angular/core';
import { edenTreaty } from '@elysiajs/eden';
import { App } from 'server';

@Injectable({
  providedIn: 'root',
})
export class ApiService {
  client = edenTreaty<App>('http://localhost:4201').api;
}
