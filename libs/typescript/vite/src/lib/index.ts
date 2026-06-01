export { angular };

import { Plugin } from 'vite';
import { DirImporterPlugin } from './dirImporterPlugin';
import { ConfigPlugin } from './configPlugin';
import { DevelopmentPlugin } from './devPlugin';
import { BuildPlugin } from './buildPlugin';
import { LinkPartialPlugin } from './linkPartialPlugin';

function angular(): Plugin[] {
  const plugins = [
    ...ConfigPlugin,
    // Link published Angular partial-declaration libs (`ɵɵngDeclare*` → AOT `ɵɵdefine*`) before
    // any other JS transform sees them, in dev (serve) and prod (build) alike. `enforce: 'pre'`
    // keeps it ahead of the dev/build transforms below.
    LinkPartialPlugin,
    DirImporterPlugin,
    DevelopmentPlugin,
    ...BuildPlugin(),
  ];
  return plugins;
}