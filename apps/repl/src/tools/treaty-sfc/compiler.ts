import { Printer } from './printer'
import { basename, extname } from 'path';
import type { Plugin } from 'vite';
import { treatyToIvy } from './treat-to-ivy';
import { loadRustCompiler } from './rust-compiler-loader';

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
	// Resolved once at startup: the Rust/NAPI `authoring_node` addon, or null with
	// an error string (in which case we fall back to the TypeScript `treatyToIvy`).
	const rust = loadRustCompiler();
	return {
		name: 'vite-plugin-template-dev',
		enforce: 'pre',
		async buildStart() {
			compiler = await loadEsmModule<
				typeof import('@angular/compiler')
			>('@angular/compiler');

			printer = Printer(compiler);

			if (rust.addon) {
				this.info(`[treaty-sfc] using Rust compiler: ${rust.path}`);
			} else {
				this.warn(
					`[treaty-sfc] Rust compiler unavailable, falling back to TypeScript treatyToIvy.\n${rust.error}`
				);
			}
		},
		config() {
			return {
				esbuild: false,
			};
		},
		async transform(code, id) {

			if (id.endsWith('.treaty')) {
				// Prefer the Rust .treaty compiler exposed via the NAPI addon.
				if (rust.addon) {
					const result = rust.addon.compileTreatyFile(code, extractFileName(id));
					for (const err of result.errors) {
						this.warn(`[treaty-sfc] ${id}: ${err}`);
					}
					return result.code;
				}
				// Fallback: TypeScript treatyToIvy path (addon failed to load).
				return treatyToIvy(code, id, compiler, extractFileName, printer);

			}
			return code;
		},
	};
}