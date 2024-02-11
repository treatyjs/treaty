import { JsonPipe } from '@angular/common';
import {
  ChangeDetectionStrategy,
  Component,
  inject,
  input,
} from '@angular/core';
import { FormBuilder, ReactiveFormsModule, Validators } from '@angular/forms';
import { ActivatedRouteSnapshot } from '@angular/router';
import { ApiService } from '../api.service';

const fb = new FormBuilder();

export const resolvePost = {
  post: async (route: ActivatedRouteSnapshot) => {
    const res = await inject(ApiService).client.id[route.params['id']].get();

    console.log(' ');
    console.log('Post resolver', res.data);
    console.log(' ');

    return res.data;
  },
};

@Component({
  selector: 'app-post',
  standalone: true,
  imports: [JsonPipe, ReactiveFormsModule],
  template: `
    <h1>Post</h1>
    @if (post !== null) {
    {{ post | json }}
    } @else {
    <p>Loading....</p>
    }
    <!-- @if (post()) {
    <pre>{{ post() | json }}</pre>

    <p>{{ post()?.data }}</p>
    } @else {
    <p>Loading....</p>
    } -->

    <form [formGroup]="form" (submit)="submit()">
      <input type="text" formControlName="strField" />
      <input type="number" formControlName="numbField" />
      <button type="submit">submit</button>
    </form>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export default class PostComponent {
  private api = inject(ApiService);

  post = input.required();
  // @Input('post') post: any;

  // id = input.required<string>();

  title = 'treaty';

  form = fb.group({
    strField: fb.control('', Validators.required),
    numbField: fb.control<number | null>(null),
  });

  async submit() {
    if (this.form.invalid) return;

    const res = await this.api.client.form.post(
      this.form.value as {
        strField: string;
        numbField: number;
      }
    );

    console.log('res: ', res);
  }
}
