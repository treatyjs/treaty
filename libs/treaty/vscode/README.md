# Treaty for VS Code

Best-in-class language support for [Treaty](https://github.com/treatyjs/treaty)
authoring formats and Angular, powered by the Treaty language server.

## Features

- **Full language intelligence** for `.treaty`, `.tjsx`, and the plain Angular
  files (`.ts`, `.html`) in your workspace — all backed by the Treaty language
  server (`@treaty/lsp`, a [Volar.js](https://volarjs.dev) server):
  - Completion (with selectorless component / `use:` directive auto-import)
  - Hover, go-to-definition, find-all-references, rename
  - Signature help and signals-aware member hints
  - Semantic tokens
  - Document & on-type formatting (participates in format-on-save)
  - Live diagnostics straight from the Rust authoring compiler, registry-aware
    so cross-module selectors resolve the way a real build would
- **Syntax highlighting** for Treaty single-file components (`.treaty`) and
  Treaty JSX (`.tjsx`), highlighting every embedded region of a `.treaty` file
  — the top ` ``` ` macro block (TypeScript), the `<style>` block (CSS), the
  TypeScript-by-default body, and the JSX/HTML template with its `{{ … }}`
  interpolations, bindings and `@if`/`@for`/`@switch`/`@defer` control flow.
- **Snippets** for the common Treaty shapes: component / directive scaffolds,
  `signal`/`computed`/`effect`, `input`/`output`/`model`, the
  `@if`/`@for`/`@switch`/`@defer`/`@let` control-flow blocks, bindings, `use:`
  directives, and a directive `host{}` spec.
- **Commands** that drive the Treaty compiler:
  - **Treaty: Preview Compiled Output** (`Ctrl/Cmd+K V`) — compile the active
    `.treaty`/`.tjsx` file and open its Ivy output beside it (plus any extracted
    server module).
  - **Treaty: Compile to Ivy** — write the compiled `.ivy.js` (and `.server.js`)
    next to the source file.
  - **Treaty: Restart Language Server**.

## Languages

| Language     | Extension  | Scope          |
| ------------ | ---------- | -------------- |
| `treaty`     | `.treaty`  | `source.treaty`|
| `treaty-jsx` | `.tjsx`    | `source.tjsx`  |

Plain Angular `.ts` and `.html` files are served by the same language server
but keep their built-in VS Code language identities — Treaty does not claim the
`.tsx` extension, so it never overrides VS Code's built-in TypeScript React.

## Settings

| Setting                | Default | Description                                            |
| ---------------------- | ------- | ------------------------------------------------------ |
| `treaty.format.enable` | `true`  | Enable the Treaty document formatter (format-on-save). |
| `treaty.trace.server`  | `off`   | Trace VS Code ⇄ Treaty language-server traffic.        |
| `treaty.server.path`   | `""`    | Path to a dev `dist/server.mjs` to use instead.        |

## How it works

The extension **bundles the Treaty language server** as `dist/server.mjs`,
launched in a child process over IPC on activation. The Rust authoring compiler
is reached through its native NAPI addon (`@treaty/authoring-node`), which the
server (and the compile/preview commands) load at runtime — the native binary is
never bundled into a JS bundle.

## Build & package

```sh
# from libs/treaty/vscode
node scripts/build.mjs   # bundle extension + server into dist/ (esbuild)
vsce package             # produce a .vsix
```

During development use `node scripts/build.mjs --watch`.

## Requirements

- VS Code `^1.88.0`
- Node `>=20.19` or Bun `>=1.1` (the runtime the bundled server runs on)
