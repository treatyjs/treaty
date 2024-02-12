import { Injectable } from '@angular/core';
import { App } from 'server';
import { edenClient } from '../libs/edenclient';

@Injectable({
  providedIn: 'root',
})
export class ApiService {
  client = edenClient<App>('http://localhost:4201').api;
}
