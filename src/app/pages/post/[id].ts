import { JsonPipe } from '@angular/common';
import { HttpClient } from '@angular/common/http';
import {
  ChangeDetectionStrategy,
  Component,
  Input,
  inject,
  input,
} from '@angular/core';
import { FormBuilder, ReactiveFormsModule, Validators } from '@angular/forms';
import { ActivatedRouteSnapshot } from '@angular/router';
import { map } from 'rxjs';
import { ApiService } from '../../api.service';
import { RoutingMeta } from 'tools/vite/routing';

export const resolvePost = {
  post: (route: ActivatedRouteSnapshot) => {
    const res = inject(ApiService)
      .client.id[route.params['id']].get()
      .pipe(
        map((test) => {
          console.log(' ');
          console.log('Post resolver', test);
          console.log(' ');

          return test.data;
        })
      );

    return res;
  },
  postOldInput: (route: ActivatedRouteSnapshot) => {
    const res = inject(ApiService)
      .client.id[route.params['id']].get()
      .pipe(
        map((test) => {
          console.log(' ');
          console.log('Post resolver', test);
          console.log(' ');

          return test.data;
        })
      );

    return res;
  },
};

export const routerMeta: RoutingMeta = {
  resolve: {
    ...resolvePost,
  },
}

const fb = new FormBuilder();


@Component({
  selector: 'app-post',
  standalone: true,
  imports: [JsonPipe, ReactiveFormsModule],
  template: `
    <h1>Post</h1>
    {{ postOldInput }}
    {{ post() }}

    <form [formGroup]="form" (submit)="submit()">
      <input type="text" formControlName="strField" />
      <input type="number" formControlName="numbField" />
      <button type="submit">submit</button>
    </form>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export default class PostComponent {
  private api = inject(HttpClient);

  post = input();
  @Input('postOldInput') postOldInput!: string;

  title = 'treaty';
  form = fb.group({
    strField: fb.control('', Validators.required),
    numbField: fb.control<number | null>(null),
  });

  async submit() {
    if (this.form.invalid) return;

    const res = this.api
      .post(
        '/api/form',
        this.form.value as {
          strField: string;
          numbField: number;
        }
      )
      .subscribe((res) => console.log('res: ', res));
  }
}
