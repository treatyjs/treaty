# Treaty for VS Code

Language support for [Treaty](https://github.com/treaty) authoring formats and
Angular, powered by the Treaty language server.

## Features

- **Syntax highlighting** for Treaty single-file components (`.treaty`) and
  Treaty JSX (`.tjsx`).
- **Full language intelligence** — diagnostics, completion, hover, and
  navigation — for `.treaty`, `.tjsx`, and the plain Angular files (`.ts`,
  `.html`) in your workspace. Embedded code in each authoring format is
  projected into a real TypeScript language service, and the Rust authoring
  compiler's diagnostics are layered on top.

This extension **bundles the Treaty language server** ([`@treaty/lsp`](../lsp)),
a [Volar.js](https://volarjs.dev)-based server. The server is launched in a
child process over IPC on activation; the Rust authoring compiler is reached
through its native NAPI addon, which the server loads at runtime (it is never
bundled into the extension).

## Languages

| Language     | Extension  | Scope          |
| ------------ | ---------- | -------------- |
| `treaty`     | `.treaty`  | `source.treaty`|
| `treaty-jsx` | `.tjsx`    | `source.tjsx`  |

Plain Angular `.ts` and `.html` files are served by the same language server
but keep their built-in VS Code language identities — Treaty does not claim the
`.tsx` extension, so it never overrides VS Code's built-in TypeScript React.

## Install

Install from the VS Code Marketplace, or build and install locally:

```sh
# from libs/treaty/vscode
npm run build        # bundle src/extension.ts -> dist/extension.js (esbuild)
npm run package      # produce a .vsix via @vscode/vsce
code --install-extension treaty-*.vsix
```

During development:

```sh
npm run watch        # rebuild on change
```

## Requirements

- VS Code `^1.88.0`
- Node `>=20.19` or Bun `>=1.1` (the runtime the bundled server runs on)
