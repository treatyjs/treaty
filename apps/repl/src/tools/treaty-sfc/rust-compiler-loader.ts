import { createRequire } from 'module';
import { existsSync } from 'fs';
import { dirname, join, resolve } from 'path';
import { fileURLToPath } from 'url';

/** Shape of a compiled component returned by the Rust/NAPI `authoring_node` addon. */
export interface CompiledComponent {
	code: string;
	errors: string[];
}

/** Subset of the native addon API the REPL relies on. */
export interface AuthoringNodeAddon {
	compileTreatyFile(source: string, fileName: string): CompiledComponent;
	compileComponent(template: string, selector: string, className: string): CompiledComponent;
	compileComponentSource(source: string): CompiledComponent;
}

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));

/**
 * Candidate locations for the prebuilt native binding. We `require` the platform
 * `.node` file directly rather than going through `libs/authoring/node/index.js`,
 * whose auto-generated re-exports may lag behind the addon's actual API.
 */
function candidatePaths(): string[] {
	const { platform, arch } = process;
	const triple =
		platform === 'win32' && arch === 'x64'
			? 'win32-x64-msvc'
			: platform === 'darwin'
				? `darwin-${arch}`
				: `${platform}-${arch}-gnu`;
	const fileName = `authoring_node.${triple}.node`;

	// Walk up from this module to the repo root, looking under libs/authoring/node.
	const roots = new Set<string>();
	let dir = here;
	for (let i = 0; i < 8; i++) {
		roots.add(join(dir, 'libs', 'authoring', 'node'));
		const parent = dirname(dir);
		if (parent === dir) break;
		dir = parent;
	}
	roots.add(resolve(here, '../../../../../libs/authoring/node'));

	return [...roots].map((r) => join(r, fileName));
}

let cached: { addon: AuthoringNodeAddon | null; path: string | null; error: string | null } | null = null;

/**
 * Load the Rust `authoring_node` NAPI addon. Returns the addon plus the resolved
 * binding path on success, or a non-null `error` describing why it could not load
 * (the caller is expected to fall back to the TypeScript compiler in that case).
 */
export function loadRustCompiler(): {
	addon: AuthoringNodeAddon | null;
	path: string | null;
	error: string | null;
} {
	if (cached) return cached;

	const tried: string[] = [];
	for (const p of candidatePaths()) {
		if (!existsSync(p)) {
			tried.push(`${p} (missing)`);
			continue;
		}
		try {
			const addon = require(p) as Partial<AuthoringNodeAddon>;
			if (typeof addon.compileTreatyFile !== 'function') {
				tried.push(`${p} (no compileTreatyFile export)`);
				continue;
			}
			cached = { addon: addon as AuthoringNodeAddon, path: p, error: null };
			return cached;
		} catch (e) {
			tried.push(`${p} (${(e as Error).message})`);
		}
	}

	cached = {
		addon: null,
		path: null,
		error: `authoring_node native addon not loadable; tried:\n  ${tried.join('\n  ')}`,
	};
	return cached;
}
