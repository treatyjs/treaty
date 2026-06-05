# treaty-shadcn

> Welcome to the world. 👋

A small, shadcn-style **Angular component library** — except not a single
component is written in classic Angular. Each one is authored in a different
front-end and **all of them compile to the same Angular Ivy output** through the
Treaty Rust/OXC compiler:

- **`.treaty` single-file components** — a TS-by-default body, interleaved
  tag-HTML (no `<template>` wrapper), and a `<style lang="scss">` block;
- **Treaty JSX (`.tsx`)** — a plain function returning JSX, lowered straight to
  a standalone signal-based Ivy component;
- **plain React (`.tsx`)** — ordinary React (`useState`, named handlers,
  destructured props) that Treaty lowers to Angular *before* its
  signals-by-default pass runs (the `react` import is stripped — it is **not**
  React at runtime).

Treaty defaults to **selectorless + standalone + signal + OnPush**, so none of
the sources below carry a `selector`, `standalone: true`, or change-detection
boilerplate — the compiler fills those in, deriving the class/selector from the
file name (`button.tsx` → `Button`).

## Components

| Component | Source | Authoring form | Surface |
| --- | --- | --- | --- |
| `Button` | `src/button.tsx` | Treaty JSX | Signal inputs `variant` (`'default' \| 'outline' \| 'ghost' \| 'destructive'`), `size` (`'sm' \| 'md' \| 'lg'`), `disabled`, `label`. Presentational; native click bubbles to the host. |
| `Badge` | `src/badge.treaty` | `.treaty` SFC | Signal inputs `variant` (`'default' \| 'secondary' \| 'destructive' \| 'outline'`), `label`. Renders one variant-styled `<span>`. |
| `Card` | `src/card.tsx` | Plain React | Props `title`, `description?` → signal inputs; `useState` `expanded` toggled by a named `onClick` handler; conditional `description` lowers to an Ivy `@if`. |
| `Alert` | `src/alert.tsx` | Plain React | Props `type` (`'info' \| 'warning' \| 'error' \| 'success'`), `title`, `message`, `isDismissible?`; `useState` `dismissed` + a named dismiss handler; the whole alert lives inside a `@if`. |
| `Input` | `src/input.tsx` | Treaty JSX | Signal inputs `type` (`'text' \| 'email' \| 'password'`), `placeholder`, `disabled`, `value`. Presentational `<input>`. |
| `Switch` | `src/switch.treaty` | `.treaty` SFC | Signal inputs `checked`, `disabled`, `label`; a local `state` signal mirrored from `checked`, flipped by a named `(click)` handler. |

## Consuming

The library ships as an Angular Package Format dist with a primary entry plus a
secondary entry per component, so you can import the whole surface or a single
component:

```ts
// Everything from the primary entry:
import { Button, Badge, Card, Alert, Input, Switch } from 'treaty-shadcn'

// Or a single component from its own subpath (tree-shakeable):
import { Button } from 'treaty-shadcn/button'
```

Because Treaty components are **selectorless**, a consumer references them by
value in `imports` (auto-import by class name) rather than by a string selector:

```ts
import { Component } from '@angular/core'
import { Button, Switch } from 'treaty-shadcn'

@Component({
  imports: [Button, Switch],
  template: `
    <Button variant="outline" size="lg" label="Save" (click)="save()" />
    <Switch [checked]="enabled()" label="Notifications" />
  `,
})
export class SettingsPanel {
  /* ... */
}
```

## Building

This library is packaged by **treaty_packagr** (the OXC-powered ng-packagr
alternative). The build is driven by `treaty-package.json`, which names the
primary entry (`src/public-api.ts`) and lists one secondary entry point per
component; packagr compiles each authoring source to Ivy `.mjs` + `.d.ts`,
writes the APF `package.json` `exports` map, and copies `README.md` into `dist/`.
