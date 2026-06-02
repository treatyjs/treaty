export { angular };

import { Plugin } from 'vite';
import { DirImporterPlugin } from './dirImporterPlugin';
import { ConfigPlugin } from './configPlugin';

// NOTE: the REPL compiles EXCLUSIVELY with the Treaty compiler (see
// `tools/treaty-compiler-plugin.ts`). The old `DevelopmentPlugin` (in-browser
// `@angular/compiler` JIT + swc) and the bitrotted AOT `BuildPlugin` are no longer
// wired — only Vite config + the directory-import resolution shim remain here.
function angular(): Plugin[] {
  const plugins = [
    ...ConfigPlugin,
    DirImporterPlugin,
  ];
  return plugins;
}