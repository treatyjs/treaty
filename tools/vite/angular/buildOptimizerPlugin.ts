export { BuildOptimizerPlugin };

import angularApplicationPreset from '@angular-devkit/build-angular/src/tools/babel/presets/application.js';
import { loadEsmModule } from '@angular-devkit/build-angular/src/utils/load-esm.js';
import { transformAsync } from '@babel/core';
import { Plugin } from 'vite';
import { requiresLinking } from './requiresLinking';

const BuildOptimizerPlugin: Plugin = {
  name: 'vite-plugin-angular-optimizer',
  apply(config, env) {
    return env.command === 'build';
  },
  enforce: 'post',
  config() {
    return {
      esbuild: {
        legalComments: 'none',
        keepNames: false,
        define: {
          ngDevMode: 'false',
          ngJitMode: 'false',
          ngI18nClosureMode: 'false',
        },
        supported: {
          // Native async/await is not supported with Zone.js. Disabling support here will cause
          // esbuild to downlevel async/await to a Zone.js supported form.
          'async-await': true,
          'async-generator': true,
          'for-await': true,
          'class-field': true,
          'class-static-field': true,
        },
      },
    };
  },
  async transform(code, id) {
    if (/\.[cm]?js$/.test(id)) {
      const angularPackage = /[\\/]node_modules[\\/]@angular[\\/]/.test(id);

      const linkerPluginCreator = (
        await loadEsmModule<
          typeof import('@angular/compiler-cli/linker/babel')
        >('@angular/compiler-cli/linker/babel')
      ).createEs2015LinkerPlugin;

      const forceAsyncTransformation =
        !/[\\/][_f]?esm2015[\\/]/.test(id) &&
        /for\s+await\s*\(|async\s+function\s*\*/.test(code);
      const shouldLink = await requiresLinking(id, code);
      const useInputSourcemap = false;

      const result = await transformAsync(code, {
        filename: id,
        inputSourceMap: useInputSourcemap,
        sourceMaps: false,
        compact: false,
        configFile: false,
        babelrc: false,
        browserslistConfigFile: false,
        plugins: [],
        presets: [
          [
            angularApplicationPreset,
            {
              angularLinker: {
                shouldLink,
                jitMode: false,
                linkerPluginCreator,
              },
              forceAsyncTransformation,
              optimize: {
                looseEnums: angularPackage,
                pureTopLevel: angularPackage,
              },
            },
          ],
        ],
      });

      return {
        code: result?.code || '',
        map: result?.map,
      };
    }
  },
};