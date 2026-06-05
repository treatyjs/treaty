import { basename, extname } from 'path';
import type { Plugin } from 'vite';
import { loadRustCompiler } from './rust-compiler-loader';

function extractFileName(filePath: string) {
	const fileName = basename(filePath, extname(filePath));
	return fileName;
}

/**
 * Vite plugin compiling `.treaty` SFCs through the ONE Treaty compiler — the
 * Rust/OXC `authoring_node` NAPI addon. There is no TypeScript fallback: if the
 * native addon cannot be loaded the build fails loudly, because the legacy TS
 * `treatyToIvy` path has been deleted.
 */
export const treatySFC: () => Plugin = () => {
	// Resolved once at startup: the Rust/NAPI `authoring_node` addon (or an error).
	const rust = loadRustCompiler();
	return {
		name: 'vite-plugin-template-dev',
		enforce: 'pre',
		buildStart() {
			if (!rust.addon) {
				this.error(
					`[treaty-sfc] Rust compiler (authoring_node NAPI addon) is required and ` +
						`could not be loaded. The legacy TypeScript fallback has been removed.\n${rust.error}`
				);
				return;
			}
			this.info(`[treaty-sfc] using Rust compiler: ${rust.path}`);
		},
		config() {
			return {
				esbuild: false,
			};
		},
		transform(code, id) {
			if (!id.endsWith('.treaty')) return code;

			if (!rust.addon) {
				this.error(
					`[treaty-sfc] cannot compile ${id}: Rust addon unavailable and there is ` +
						`no TypeScript fallback.\n${rust.error}`
				);
				return code;
			}

			const result = rust.addon.compileTreatyFile(code, extractFileName(id));
			for (const err of result.errors) {
				this.warn(`[treaty-sfc] ${id}: ${err}`);
			}
			return result.code;
		},
	};
}
