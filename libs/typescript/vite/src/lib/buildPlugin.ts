export { BuildPlugin };

import angularApplicationPreset from '@angular-devkit/build-angular/src/tools/babel/presets/application';
import { CompilerPluginOptions } from '@angular-devkit/build-angular/src/tools/esbuild/angular/compiler-plugin';
import { JavaScriptTransformer } from '@angular-devkit/build-angular/src/tools/esbuild/javascript-transformer';
import {
  type CompilerHost,
  type NgtscProgram,
} from '@angular/compiler-cli';
import { transformAsync } from '@babel/core';
import {
  mergeTransformers,
  replaceBootstrap,
} from '@ngtools/webpack/src/ivy/transformation';
import * as ts from 'typescript';
import {
  DepOptimizationConfig,
  normalizePath,
  Plugin,
  PluginContainer,
} from 'vite';
import { BuildOptimizerPlugin } from './buildOptimizerPlugin';
import { getGlobalConfig } from './configPlugin';
import { loadEsmModule } from '@angular-devkit/build-angular/src/utils/load-esm';
import { readFile, readFileSync } from 'fs';

interface EmitFileResult {
  code: string;
  map?: string;
  dependencies: readonly string[];
  hash?: Uint8Array;
}
type FileEmitter = (file: string) => Promise<EmitFileResult | undefined>;

const BuildPlugin = (): Plugin[] => {
  let tsconfigPath = '';
  let rootNames: string[] = [];
  let compilerOptions: any = {};
  let host: ts.CompilerHost;
  let fileEmitter: FileEmitter | undefined;
  let cssPlugin: Plugin | undefined;
  let complierCli: typeof import('@angular/compiler-cli');

  async function buildAndAnalyze() {
    const angularProgram: NgtscProgram = new complierCli.NgtscProgram(
      rootNames,
      compilerOptions,
      host as CompilerHost,
    );

    const angularCompiler = angularProgram.compiler;
    const typeScriptProgram = angularProgram.getTsProgram();
    const builder = ts.createAbstractBuilder(typeScriptProgram, host);
    await angularCompiler.analyzeAsync();
    const diagnostics = angularCompiler.getDiagnostics();

    const msg = ts.formatDiagnosticsWithColorAndContext(diagnostics, host);
    if (msg) {
      return msg;
    }

    fileEmitter = createFileEmitter(
      builder,
      mergeTransformers(angularCompiler.prepareEmit().transformers, {
        before: [replaceBootstrap(() => builder.getProgram().getTypeChecker())],
      }),
      () => [],
    );
    return;
  }

  return [
    {
      name: 'vite-plugin-angular-prod-post',
      enforce: 'post',
      apply(config, env) {
        // return env.command === 'serve';
        return true
      },
      async config(_userConfig, env) {
        complierCli = await loadEsmModule<
            typeof import('@angular/compiler-cli')
        >('@angular/compiler-cli');

        const { root, workspaceRoot } = getGlobalConfig();

        //TODO: users may name it tsconfig.app.json(angular cli convention)
        tsconfigPath = complierCli.join(root!, 'tsconfig.app.json');
        return {
          optimizeDeps: {
            include: ['rxjs/operators', 'rxjs'],
            exclude: ['@angular/platform-server'],
            esbuildOptions: {
              plugins: [
                createCompilerPlugin({
                  tsconfig: tsconfigPath,
                  sourcemap: false,
                  advancedOptimizations: true,
                  incremental: true,
                }),
              ],
              define: {
                ngDevMode: 'false',
                ngJitMode: 'false',
                ngI18nClosureMode: 'false',
              },
            },
          },
        };
      },
    },
    {
      name: 'vite-plugin-angular-prod',
      enforce: 'pre',
      apply(config, env) {
        // return env.command === 'build';
    return false
      },
      async transform(code, id) {
        if (id.includes('node_modules')) {
          return;
        }

        if (/\.[cm]?ts?$/.test(id)) {
          const result = await fileEmitter!(id);
          const data = result?.code ?? '';
          const forceAsyncTransformation =
            /for\s+await\s*\(|async\s+function\s*\*/.test(data);
          const babelResult = await transformAsync(data, {
            filename: id,
            inputSourceMap: null,
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
                  forceAsyncTransformation,
                  optimize: {},
                },
              ],
            ],
          });

          return {
            code: babelResult?.code ?? '',
            map: babelResult?.map,
          };
        }

        return undefined;
      },
      async buildStart({ plugins }) {
        const { options: tsCompilerOptions, rootNames: rn } = complierCli.readConfiguration(
          tsconfigPath,
          {
            compilationMode: 'full',
            suppressOutputPathCheck: true,
            outDir: undefined,
            inlineSourceMap: false,
            inlineSources: false,
            declaration: false,
            declarationMap: false,
            allowEmptyCodegenFiles: false,
            annotationsAs: 'decorators',
            enableResourceInlining: false,
            supportTestBed: false,
          },
        );

        rootNames = rn;
        compilerOptions = tsCompilerOptions;
        host = ts.createIncrementalCompilerHost(compilerOptions);

        if (Array.isArray(plugins)) {
          cssPlugin = plugins.find(plugin => plugin.name === 'vite:css');
        }
        augmentHostWithResources(
          host,
          cssPlugin!.transform as PluginContainer['transform'],
          {
            inlineStylesExtension: 'scss',
          },
        );

        const msg = await buildAndAnalyze();
        if (msg) {
          console.log(msg);
          process.exit(1);
        }
      },
    },
    BuildOptimizerPlugin,
  ];
};

function createFileEmitter(
  program: ts.BuilderProgram,
  transformers: ts.CustomTransformers = {},
  onAfterEmit?: (sourceFile: ts.SourceFile) => void,
): FileEmitter {
  return async (file: string) => {
    const sourceFile = program.getSourceFile(file);
    if (!sourceFile) {
      return undefined;
    }

    let code: string = '';
    program.emit(
      sourceFile,
      (filename: string, data: string) => {
        if (/\.[cm]?js$/.test(filename)) {
          if (data) {
            code = data;
          }
        }
      },
      undefined /* cancellationToken */,
      undefined /* emitOnlyDtsFiles */,
      transformers,
    );

    onAfterEmit?.(sourceFile);

    return { code, dependencies: [] };
  };
}

function augmentHostWithResources(
  host: ts.CompilerHost,
  transform: (
    code: string,
    id: string,
    options?: { ssr?: boolean },
  ) => ReturnType<any> | null,
  options: {
    inlineStylesExtension?: string;
  } = {},
) {
  const resourceHost = host as CompilerHost;

  resourceHost.readResource = function (fileName: string) {
    const filePath = normalizePath(fileName);

    const content = readFileSync(filePath, 'utf-8');
    if (content === undefined) {
      throw new Error('Unable to locate component resource: ' + fileName);
    }

    return content;
  };

  resourceHost.transformResource = async function (data, context) {
    // Only style resources are supported currently
    if (context.type !== 'style') {
      return null;
    }

    if (options.inlineStylesExtension) {
      // Resource file only exists for external stylesheets
      const filename =
        context.resourceFile ??
        `${context.containingFile.replace(
          /\.ts$/,
          `.${options?.inlineStylesExtension}`,
        )}`;

      let stylesheetResult;

      try {
        stylesheetResult = await transform(data, `${filename}?direct`);
      } catch (e) {
        console.error(`${e}`);
      }

      return { content: stylesheetResult?.code || '' };
    }

    return null;
  };
}

type EsbuildOptions = NonNullable<DepOptimizationConfig['esbuildOptions']>;
type EsbuildPlugin = NonNullable<EsbuildOptions['plugins']>[number];
function createCompilerPlugin(
  pluginOptions: CompilerPluginOptions,
): EsbuildPlugin {
  const javascriptTransformer = new JavaScriptTransformer(pluginOptions, 1);

  return {
    name: 'vite-plugin-angular-deps-optimizer',
    async setup(build) {
      build.onLoad({ filter: /\.[cm]?js$/ }, async args => {
        const contents = await javascriptTransformer.transformFile(args.path);

        return {
          contents,
          loader: 'js',
        };
      });

      build.onEnd(() => javascriptTransformer.close());
    },
  };
}