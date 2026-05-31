/**
 * @module
 *
 * The framework-agnostic core of `@treaty/module-federation`: it turns a small,
 * declarative description of a Treaty app (its name, the remotes it consumes,
 * the modules it exposes, and the deps it shares) into a single **normalized**
 * Module Federation config. That normalized shape is the one true source the
 * Rspack and Vite adapters translate into bundler-specific plugin options.
 *
 * Treaty is a compiler, not a host: the developer never writes a
 * `ModuleFederationPlugin` by hand. Every Treaty app is a Module Federation
 * host automatically and can also expose remotes — the defaults here encode
 * that principle. In particular, the Angular runtime packages
 * (`@angular/core`, `@angular/common`, …) plus `rxjs`/`tslib`/`zone.js` are
 * shared as **eager singletons** so a federated Angular graph has exactly one
 * copy of the framework at runtime.
 */

import {
	deriveExposesFromLibs,
	deriveExposesFromRoutes,
	type DeriveLibsOptions,
	type DeriveRoutesOptions,
	type LibEntry,
	type RouteLike,
} from './routes.js'

/**
 * Where a remote's entry manifest lives. Either a bare URL string (the common
 * case) or a `{ name, entry }` pair when the remote's federation name differs
 * from the local alias used in `import('alias/Thing')`.
 */
export type RemoteEntry =
	| string
	| {
			/** The remote container's federation `name` (its global/global-scope key). */
			readonly name: string
			/** URL of the remote's entry manifest (e.g. `remoteEntry.js` / `mf-manifest.json`). */
			readonly entry: string
	  }

/**
 * The simple, developer-facing options accepted by {@link generateMfConfig}.
 * Every field is optional: with `{}` you still get a valid host config that
 * shares the Angular singletons — the zero-config default.
 */
export interface MfOptions {
	/**
	 * This app's federation name. Used as the container `name` and as the UMD
	 * library name. Defaults to {@link DEFAULT_HOST_NAME} (`"app"`) when omitted —
	 * a single standalone app needs no explicit name to still be a valid host.
	 */
	readonly name?: string
	/**
	 * The filename of this app's own remote entry, so other apps can consume it.
	 * Because every Treaty app is a host that can also expose remotes, this is
	 * always emitted. Defaults to {@link DEFAULT_FILENAME}.
	 */
	readonly filename?: string
	/**
	 * Remotes this app consumes, keyed by the local alias used in dynamic imports
	 * (`import('dashboard/Widget')` → key `"dashboard"`). The value is the remote
	 * entry URL (or a `{ name, entry }` pair). Empty/omitted ⇒ a pure host.
	 */
	readonly remotes?: Readonly<Record<string, RemoteEntry>>
	/**
	 * Modules this app exposes to other apps, keyed by the public path consumers
	 * import (`"./Widget"` → `"./src/app/widget.ts"`). Empty/omitted ⇒ the app
	 * exposes nothing but is still a valid host.
	 */
	readonly exposes?: Readonly<Record<string, string>>
	/**
	 * Extra packages to share, merged over the Angular defaults. A `true` value
	 * uses the default eager-singleton policy; an object overrides individual
	 * fields (e.g. `{ rxjs: { singleton: true, eager: false } }`).
	 */
	readonly shared?: Readonly<Record<string, SharedConfig | true>>
	/**
	 * Replace (rather than extend) the built-in Angular shared defaults. When
	 * `false`, no Angular singletons are injected and only {@link MfOptions.shared}
	 * entries are shared. Defaults to `true`. Most apps should leave this on.
	 */
	readonly shareAngular?: boolean
	/**
	 * The semver range applied to the auto-shared Angular packages. Defaults to
	 * {@link DEFAULT_ANGULAR_VERSION}. Use this to pin the federated framework
	 * version across a set of apps that must agree on one Angular copy.
	 */
	readonly angularVersion?: string
	/**
	 * The app's Angular routes config. When provided, every **lazy** route
	 * (`loadComponent`/`loadChildren`) is auto-derived into an `exposes` entry so
	 * the app exposes each lazy feature as an independently deployable remote
	 * with no manual `exposes`. See {@link deriveExposesFromRoutes}. Manual
	 * {@link MfOptions.exposes} entries win over derived ones.
	 */
	readonly routes?: readonly RouteLike[]
	/**
	 * Workspace libraries to expose as federated modules — each becomes one
	 * independently versioned/deployable remote. See {@link deriveExposesFromLibs}.
	 * Manual {@link MfOptions.exposes} entries win over derived ones.
	 */
	readonly libs?: readonly LibEntry[]
	/** Tune how {@link MfOptions.routes} map to expose keys/paths. */
	readonly routesOptions?: DeriveRoutesOptions
	/** Tune how {@link MfOptions.libs} map to expose keys. */
	readonly libsOptions?: DeriveLibsOptions
}

/** Per-package sharing policy, mirroring the Module Federation `shared` entry. */
export interface SharedConfig {
	/** Force a single shared instance across the whole federated graph. */
	readonly singleton?: boolean
	/** Load the shared module up front with the host rather than on demand. */
	readonly eager?: boolean
	/** The version this app provides (used for negotiation). */
	readonly version?: string
	/** The minimum version this app will accept from the shared scope. */
	readonly requiredVersion?: string
	/** Fail hard instead of falling back when the singleton version mismatches. */
	readonly strictVersion?: boolean
}

/**
 * The normalized, fully-resolved config produced by {@link generateMfConfig}.
 * Unlike {@link MfOptions} every field is present and every `shared` entry is a
 * concrete {@link SharedConfig}, so adapters never have to re-apply defaults.
 */
export interface NormalizedMfConfig {
	/** The container/library name for this app. */
	readonly name: string
	/** This app's own remote-entry filename. */
	readonly filename: string
	/** Consumed remotes, keyed by local alias → `{ name, entry }`. */
	readonly remotes: Readonly<Record<string, { readonly name: string; readonly entry: string }>>
	/** Exposed modules, keyed by public path → local module path. */
	readonly exposes: Readonly<Record<string, string>>
	/** Fully-resolved shared policy, keyed by package name. */
	readonly shared: Readonly<Record<string, SharedConfig>>
}

/** Default federation name when an app does not supply one. */
export const DEFAULT_HOST_NAME = 'app'

/** Default remote-entry filename emitted for every app (so it can be consumed). */
export const DEFAULT_FILENAME = 'remoteEntry.js'

/**
 * Default semver range for the auto-shared Angular packages. A caret range so
 * patch/minor releases satisfy the singleton while still pinning the major.
 */
export const DEFAULT_ANGULAR_VERSION = '^21.0.0'

/**
 * The Angular runtime packages Treaty shares as eager singletons by default.
 * One copy of each of these in a federated graph is required for the framework
 * to behave (DI, zone, change detection, router state, …) — so they are eager
 * (loaded with the host) and singleton (exactly one instance).
 */
export const DEFAULT_ANGULAR_PACKAGES: readonly string[] = [
	'@angular/core',
	'@angular/common',
	'@angular/common/http',
	'@angular/compiler',
	'@angular/animations',
	'@angular/forms',
	'@angular/platform-browser',
	'@angular/platform-browser-dynamic',
	'@angular/router',
]

/**
 * Non-Angular runtime packages that must also be singletons in an Angular
 * federation: the RxJS scheduler/Subject identity, the TS runtime helpers, and
 * the zone that drives change detection all break if duplicated.
 */
export const DEFAULT_SINGLETON_PACKAGES: readonly string[] = ['rxjs', 'tslib', 'zone.js']

/** Build the default eager-singleton shared map for the given Angular version. */
function angularSharedDefaults(angularVersion: string): Record<string, SharedConfig> {
	const shared: Record<string, SharedConfig> = {}
	for (const pkg of DEFAULT_ANGULAR_PACKAGES) {
		shared[pkg] = {
			singleton: true,
			eager: true,
			requiredVersion: angularVersion,
		}
	}
	// rxjs/tslib/zone.js are versioned independently of Angular; keep them as
	// eager singletons but leave the version open for the package manager to fill.
	for (const pkg of DEFAULT_SINGLETON_PACKAGES) {
		shared[pkg] = { singleton: true, eager: true }
	}
	return shared
}

/** Normalize a {@link RemoteEntry} to its `{ name, entry }` object form. */
function normalizeRemote(alias: string, value: RemoteEntry): { name: string; entry: string } {
	if (typeof value === 'string') {
		// `alias@http://host/remoteEntry.js` is the canonical MF remote string.
		// A bare alias defaults the federation name to the local alias.
		return { name: alias, entry: value }
	}
	return { name: value.name, entry: value.entry }
}

/**
 * Resolve a single user `shared` entry to a concrete {@link SharedConfig}. A
 * `true` value adopts the eager-singleton default; an object is taken as-is.
 */
function normalizeShared(value: SharedConfig | true): SharedConfig {
	return value === true ? { singleton: true, eager: true } : value
}

/**
 * Generate the normalized Module Federation config for a Treaty app from the
 * simple {@link MfOptions}. This is the single place defaults live — both the
 * Rspack and Vite adapters consume its output, so a Treaty app behaves
 * identically across bundlers with zero developer configuration.
 *
 * Behaviour:
 *   - Always produces a valid **host** (name + filename), even from `{}`.
 *   - Shares the Angular runtime as **eager singletons** unless
 *     {@link MfOptions.shareAngular} is `false`.
 *   - Merges {@link MfOptions.shared} over the Angular defaults (user wins).
 *   - Normalizes every remote to a `{ name, entry }` pair.
 *   - Auto-derives `exposes` from {@link MfOptions.routes} (each lazy route) and
 *     {@link MfOptions.libs} (each library); manual {@link MfOptions.exposes}
 *     wins. With no `routes`/`libs`/`exposes`, `exposes` is empty — backward
 *     compatible with callers that never supplied them.
 */
export function generateMfConfig(options: MfOptions = {}): NormalizedMfConfig {
	const name = options.name ?? DEFAULT_HOST_NAME
	const filename = options.filename ?? DEFAULT_FILENAME
	const angularVersion = options.angularVersion ?? DEFAULT_ANGULAR_VERSION
	const shareAngular = options.shareAngular ?? true

	const remotes: Record<string, { name: string; entry: string }> = {}
	for (const [alias, value] of Object.entries(options.remotes ?? {})) {
		remotes[alias] = normalizeRemote(alias, value)
	}

	// Auto-derive exposes from the route/lib graph (federation = deployment
	// granularity: every lazy route + lib is an independently deployable remote),
	// then let any manual `exposes` win so an explicit override is never lost.
	const exposes: Record<string, string> = {
		...deriveExposesFromRoutes(options.routes, options.routesOptions),
		...deriveExposesFromLibs(options.libs, options.libsOptions),
		...(options.exposes ?? {}),
	}

	// Angular defaults first, then user overrides so the developer always wins.
	const shared: Record<string, SharedConfig> = shareAngular
		? angularSharedDefaults(angularVersion)
		: {}
	for (const [pkg, value] of Object.entries(options.shared ?? {})) {
		shared[pkg] = normalizeShared(value)
	}

	return { name, filename, remotes, exposes, shared }
}
