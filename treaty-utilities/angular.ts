import { OnLoadResult, plugin, type BunPlugin } from "bun";
import { NgtscProgram, setFileSystem, createCompilerHost, CompilerOptions, NodeJSFileSystem, readConfiguration } from '@angular/compiler-cli';
import { ScriptTarget, ModuleKind } from "typescript";
import { mergeTransformers, replaceBootstrap } from '@ngtools/webpack/src/ivy/transformation';
import { augmentProgramWithVersioning } from '@ngtools/webpack/src/ivy/host';
setFileSystem(new NodeJSFileSystem());
export const Angular: BunPlugin = {
	name: "Angular loader",
	async setup(build) {
		setFileSystem(new NodeJSFileSystem());
		const options: CompilerOptions = {
			strict: true,
			strictTemplates: true,
			target: ScriptTarget.Latest,
			module: ModuleKind.ESNext,
			annotateForClosureCompiler: true,
			compilationMode: "experimental-local",
		};



		const ng = new NgtscProgram(["./src/main.ts"], options, createCompilerHost({ options }));
		await ng.compiler.analyzeAsync()
		const program = ng.getReuseTsProgram();
		augmentProgramWithVersioning(program);
		build.onLoad({ filter: /.ts$/ }, ({ path }) => {
			console.log("file to build: ", path);
			const sourceFile = ng.getReuseTsProgram().getSourceFile(path);



			let content: string | undefined;
			program.emit(
				sourceFile,
				(filename, data) => {
					if (/\.[cm]?js$/.test(filename)) {
						content = data;
					}
				},
				undefined,
				undefined,
				mergeTransformers(
					{
						before: [replaceBootstrap(() => program.getTypeChecker())]
					},
					ng.compiler.prepareEmit().transformers
				)
			);

			console.log("Jordan", content)

			return { contents: content, loader: 'js' } as OnLoadResult
		})

	},
};

plugin(Angular);