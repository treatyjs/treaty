/**
 * @ngx-maintenance/registry
 *
 * The opted-in Angular library registry plus deterministic discovery of stale
 * libraries. People ADD their library (a {@link RegistryEntry}); the bot also
 * DISCOVERS libraries that are behind the latest Angular major AND have had no
 * commit in more than six months ({@link discoverStale}), then suggests they
 * opt in.
 *
 * No AI is used anywhere: discovery and staleness are pure functions of npm /
 * GitHub metadata.
 */

export type { AngularMajor } from "./types.js";
export type {
  RegistryEntry,
  RegistryManifest,
  DiscoveryCandidate,
  RepoMetadata,
  StaleFinding,
} from "./types.js";
export {
  STALE_THRESHOLD_MS,
  SIX_MONTHS_MS,
  DAY_MS,
  MANIFEST_VERSION,
} from "./types.js";

export {
  isInactive,
  isBehindLatest,
  isStale,
  evaluateCandidate,
  discoverStale,
} from "./discovery.js";

export {
  createManifest,
  findEntry,
  upsertEntry,
  removeEntry,
  markAppInstalled,
  parseManifest,
  serializeManifest,
  loadManifest,
  saveManifest,
} from "./manifest.js";
