export { angular };

import { Plugin } from 'vite';
import { DirImporterPlugin } from './dirImporterPlugin';
import { ConfigPlugin } from './configPlugin';
import { DevelopmentPlugin } from './devPlugin';
import { BuildPlugin } from './buildPlugin';

function angular(): Plugin[] {
  const plugins = [
    ...ConfigPlugin,
    DirImporterPlugin,
    DevelopmentPlugin,
    ...BuildPlugin(),
  ];
  return plugins;
}