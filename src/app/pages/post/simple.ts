---
import MyComponent from "./[id]"
import {input} from '@angular/core'

const Name = input();
const PostDetails = input();
---

<h1>{{Name}}</h1>
<MyComponent [post]="PostDetails" />
<style>
    h1 {
      background-color: var(--backgroundColor);
      color: var(--foregroundColor);
    }
</style>