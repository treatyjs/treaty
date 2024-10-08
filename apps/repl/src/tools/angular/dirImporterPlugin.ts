export { DirImporterPlugin };

import { stat } from 'fs/promises';
import { relative, resolve } from 'path';
import { cwd } from 'process';
import { normalizePath, Plugin } from 'vite';

// Workaround for Node.js [ERR_UNSUPPORTED_DIR_IMPORT]
const DirImporterPlugin: Plugin = {
  name: 'vite-plugin-angular/dir-importer',
  enforce: 'pre',
  async resolveId(source: string, importer: string | undefined, options: {
    attributes: Record<string, string>;
    custom?: any;
    ssr?: boolean;
    isEntry: boolean;
}) {
    if (!importer || !options.ssr) {
      return;
    }
    try {
      const packageName = normalizePath(relative(cwd(), source));
      const relativePath = resolve(cwd(), 'node_modules', source);
      const stats = await stat(relativePath);
      if (stats.isDirectory()) {
        const lastPathSegment = source.split('/').pop();
        const candidates = [
          'index.js',
          'index.mjs',
          lastPathSegment + '.js',
          lastPathSegment + '.mjs',
        ];

        for (const candidate of candidates) {
          try {
            const stats = await stat(resolve(relativePath, candidate));
            if (stats.isFile()) {
              return this.resolve(`${packageName}/${candidate}`, importer, {
                ...options,
                skipSelf: true,
              });
            }
          } catch {}
        }
      }
    } catch {}
    return;
  },
};