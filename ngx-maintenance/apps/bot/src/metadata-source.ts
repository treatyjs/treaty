import type { OctokitLike, RepoRef } from "@ngx-maintenance/github-adapter";
import { parseRepoRef } from "@ngx-maintenance/github-adapter";
import type {
  AngularMajor,
  MetadataSource,
  RepoMetadata,
} from "@ngx-maintenance/staleness-detector";
import type { RegistryManifest } from "@ngx-maintenance/registry";

/**
 * The octokit surface the metadata source reads through. A superset of the
 * adapter's create/list boundary that adds the read-only repo queries needed to
 * derive last-commit timestamps. Production passes a real Octokit (which carries
 * these); tests pass a fake.
 */
export interface MetadataOctokit extends OctokitLike {
  readonly rest: OctokitLike["rest"] & {
    readonly repos: {
      listCommits(
        params: Record<string, unknown>,
      ): Promise<{ data: ReadonlyArray<unknown> }>;
    };
  };
}

/** Inputs for {@link createMetadataSource}. */
export interface MetadataSourceConfig {
  /** The registry manifest: the opted-in libraries to evaluate as candidates. */
  readonly manifest: RegistryManifest;
  /** The current latest published Angular major (from the npm registry). */
  readonly latestAngular: AngularMajor;
}

/** Read the most recent commit's epoch-ms timestamp for a repository. */
async function lastCommitMs(
  octokit: MetadataOctokit,
  ref: RepoRef,
): Promise<number> {
  const response = await octokit.rest.repos.listCommits({
    owner: ref.owner,
    repo: ref.repo,
    per_page: 1,
  });
  const [latest] = response.data;
  if (typeof latest !== "object" || latest === null) return 0;
  const commit = (latest as Record<string, unknown>)["commit"];
  if (typeof commit !== "object" || commit === null) return 0;
  const committer = (commit as Record<string, unknown>)["committer"];
  if (typeof committer !== "object" || committer === null) return 0;
  const date = (committer as Record<string, unknown>)["date"];
  if (typeof date !== "string") return 0;
  const parsed = Date.parse(date);
  return Number.isNaN(parsed) ? 0 : parsed;
}

/**
 * Build a production {@link MetadataSource} over the registry manifest: each
 * opted-in entry is a candidate, its current Angular major comes from the
 * manifest, and its last-commit timestamp is read live from GitHub through the
 * octokit boundary. The latest Angular major is supplied by config (resolved
 * from the npm registry out-of-band). All network access is confined to the
 * injected octokit, so this stays fake-testable.
 */
export function createMetadataSource(
  octokit: MetadataOctokit,
  config: MetadataSourceConfig,
): MetadataSource {
  const entries = config.manifest.entries;

  const toMetadata = async (
    npmName: string,
  ): Promise<RepoMetadata | undefined> => {
    const entry = entries.find((e) => e.npmName === npmName);
    if (entry === undefined) return undefined;
    const ref = parseRepoRef(entry.repoUrl);
    if (ref === undefined) return undefined;
    return {
      npmName: entry.npmName,
      repoUrl: entry.repoUrl,
      currentAngular: entry.currentAngular,
      lastCommitMs: await lastCommitMs(octokit, ref),
    };
  };

  return {
    fetchMetadata: (npmName) => toMetadata(npmName),
    fetchLatestAngularMajor: () => Promise.resolve(config.latestAngular),
    async listCandidates() {
      const out: RepoMetadata[] = [];
      for (const entry of entries) {
        // Sequential to keep deterministic order and avoid an API burst.
        // eslint-disable-next-line no-await-in-loop
        const metadata = await toMetadata(entry.npmName);
        if (metadata !== undefined) out.push(metadata);
      }
      return out;
    },
  };
}
