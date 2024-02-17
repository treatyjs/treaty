import { plugin, type BunPlugin } from "bun";
import { JavaScriptTransformer } from '@angular-devkit/build-angular/src/tools/esbuild/javascript-transformer'


export const AngularPlugin: BunPlugin = {
  name: "Angular Bun loader",
  setup(build) {

    const javascriptTransformer = new JavaScriptTransformer({
        sourcemap: false,
        advancedOptimizations: false,
        jit: true,
        
    },  1);
    build.onLoad({filter: /\.[cm]?[jt]s?$/ }, (async ({path}) => {
      console.log(path);
      try {
        console.log('transforming')
        const contents = await javascriptTransformer.transformFile(path);
        console.log('transforming finished')
        console.log(path);
        console.log('contents', contents);
        return {
          contents,
          loader: 'ts',
        };
      } catch (error) {
        console.log(error)
      }
    }))
  },
};

plugin(AngularPlugin);