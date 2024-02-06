import { JsonPipe } from '@angular/common';
import {
  ChangeDetectionStrategy,
  Component,
  inject,
  input,
} from '@angular/core';
import { FormBuilder, ReactiveFormsModule, Validators } from '@angular/forms';
import { computedAsync } from 'ngxtension/computed-async';
import { ApiService } from '../api.service';

const fb = new FormBuilder();

@Component({
  selector: 'app-post',
  standalone: true,
  imports: [JsonPipe, ReactiveFormsModule],
  template: `
    <h1>Post</h1>
    @if (post()) {
    <pre>{{ post() | json }}</pre>

    <p>{{ post()?.data }}</p>
    } @else {
    <p>Loading....</p>
    }

    <form [formGroup]="form" (submit)="submit()">
      <input type="text" formControlName="strField" />
      <input type="number" formControlName="numbField" />
      <button type="submit">submit</button>
    </form>
  `,
  changeDetection: ChangeDetectionStrategy.OnPush,
})
export default class PostComponent {
  private id = input.required<string>();
  private api = inject(ApiService);

  post = computedAsync(() => this.api.client.id[this.id()].get());

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
