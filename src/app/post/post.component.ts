import { JsonPipe } from '@angular/common';
import {
  ChangeDetectionStrategy,
  Component,
  inject,
  input} from '@angular/core';
import { FormBuilder, ReactiveFormsModule, Validators } from '@angular/forms';
import { HttpClient } from '@angular/common/http';
import { map } from 'rxjs';
import { ActivatedRouteSnapshot } from '@angular/router';

const fb = new FormBuilder();
export const resolvePost = {
  post: (route: ActivatedRouteSnapshot) => {
    
    const res =  inject(HttpClient).get<{data: string}>('/api/id/' + route.params['id']).pipe(map(res => {
      console.log(' ');
      console.log('Post resolver', res.data);
      console.log(' ');
        return res.data;
      }))
    

    return res;
  },
};

@Component({
  selector: 'app-post',
  standalone: true,
  imports: [JsonPipe, ReactiveFormsModule],
  template: `
    <h1>Post</h1>
    {{ post() }}
    @if (post !== null) {
      {{ post() }}
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
  private api = inject(HttpClient)

  post = input();
  

  title = 'treaty';

  form = fb.group({
    strField: fb.control('', Validators.required),
    numbField: fb.control<number | null>(null),
  });

  async submit() {
    if (this.form.invalid) return;

    const res = this.api.post('/api/form',
      this.form.value as {
        strField: string;
        numbField: number;
      }
    ).subscribe(res => console.log('res: ', res));
  }
}
