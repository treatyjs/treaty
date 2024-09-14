import { Printer } from './printer'
import { basename, extname } from 'path';
import type { Plugin } from 'vite';
import { treatyToIvy } from './treat-to-ivy';

export function loadEsmModule<T>(modulePath: string | URL): Promise<T> {
	return new Function('modulePath', `return import(modulePath);`)(
		modulePath
	) as Promise<T>;
}


function extractFileName(filePath: string) {

	const fileName = basename(filePath, extname(filePath));

	return fileName;
}


export const treatySFC: () => Plugin = () => {
	let compiler: typeof import('@angular/compiler');
	let printer: ReturnType<typeof Printer>
	return {
		name: 'vite-plugin-template-dev',
		enforce: 'pre',
		async buildStart() {
			compiler = await loadEsmModule<
				typeof import('@angular/compiler')
			>('@angular/compiler');

			printer = Printer(compiler);

		},
		config() {
			return {
				esbuild: false,
			};
		},
		async transform(code, id) {

			if (id.endsWith('.treaty')) {
				return treatyToIvy(code, id, compiler, extractFileName);
				
			}
			return code;
		},
	};
}