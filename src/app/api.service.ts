import { Injectable } from '@angular/core';
import { App } from 'server';
import { edenClient } from '../libs/edenclient/mod';

@Injectable({
  providedIn: 'root',
})
export class ApiService {
  client = edenClient<App>('http://localhost:5555').api;
}
