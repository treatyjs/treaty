import {
  createGitHubAdapter,
  parseRepoRef,
  Octokit,
  type AdapterConfig,
  type GitHubAdapter,
} from "@ngx-maintenance/github-adapter";
import type { MetadataSource } from "@ngx-maintenance/staleness-detector";
import type {
  CreateRepoResult,
  ForkInput,
  ForkResult,
  GithubAdapter as TakeoverAdapter,
  NewRepoSpec,
  PublishInput,
  PublishResult,
} from "@ngx-maintenance/takeover";
import {
  createTreatyMigrationStep,
  type TreatyMigrationStep,
} from "@ngx-maintenance/treaty-support";

/** The npm organisation / GitHub org all taken-over forks are created under. */
const FORK_ORG = "ngx-maintenance";

/**
 * The complete injectable boundary bundle the orchestrator runs against. Every
 * side effect (GitHub API, git/process, filesystem, npm registry metadata) is
 * reachable ONLY through these three handles, so {@link runMaintenanceCycle} is
 * a pure function of its inputs and fully fake-testable.
 */
export interface MaintenanceAdapters {
  /** GitHub API + git/process + workdir boundary (PRs, clone, migrate). */
  readonly github: GitHubAdapter;
  /** npm/GitHub metadata source the staleness detector reads through. */
  readonly metadata: MetadataSource;
  /** The fork/create-repo/publish boundary the takeover module drives. */
  readonly takeover: TakeoverAdapter;
  /**
   * The OPTIONAL Treaty migration boundary. Present only when the deployment
   * enables the Treaty path; the cycle calls it solely for libraries listed in
   * `config.treatyOptIn`. When omitted, no library runs the Treaty step and the
   * flow is unchanged. Injected as a fake in tests.
   */
  readonly treaty?: TreatyMigrationStep;
}

/** Configuration for the production adapter bundle. */
export interface ProductionAdaptersConfig extends AdapterConfig {
  /** The metadata source (npm registry + GitHub queries) to read through. */
  readonly metadata: MetadataSource;
  /**
   * Enable the optional Treaty migration step. When `true`, the production
   * bundle wires {@link createTreatyMigrationStep} over the GitHub adapter's
   * shell; the cycle still only runs it for `config.treatyOptIn` libraries.
   * Defaults to `false` (no Treaty boundary at all).
   */
  readonly enableTreaty?: boolean;
}

/**
 * Build the PRODUCTION takeover boundary: a real {@link Octokit} authenticated
 * with `token` creates the standalone `@ngx-maintenance/<name>` repository and
 * forks the source into it; the supplied {@link GitHubAdapter}'s shell publishes
 * the staged fork to npm. The Octokit `repos` + `request` surface used here is
 * the full client (the narrow {@link GitHubAdapter.octokit} subset deliberately
 * exposes only PR/issue create+list), so the takeover effects stay disjoint
 * from the migration-PR boundary while still going through one process shell.
 *
 * Tests do NOT call this; they inject a fake {@link TakeoverAdapter} directly.
 */
export function createTakeoverAdapter(
  token: string,
  github: GitHubAdapter,
): TakeoverAdapter {
  const octokit = new Octokit({ auth: token });
  return {
    async createRepo(spec: NewRepoSpec): Promise<CreateRepoResult> {
      const response = await octokit.request("POST /orgs/{org}/repos", {
        org: FORK_ORG,
        name: spec.name,
        description: spec.description,
        private: !spec.isPublic,
      });
      const data = response.data as { html_url?: string } | undefined;
      return {
        repoUrl:
          data?.html_url ?? `https://github.com/${FORK_ORG}/${spec.name}`,
      };
    },
    async fork(input: ForkInput): Promise<ForkResult> {
      const ref = parseRepoRef(input.sourceRepoUrl);
      if (ref === undefined) {
        throw new Error(`cannot parse repo from URL: ${input.sourceRepoUrl}`);
      }
      const response = await octokit.rest.repos.createFork({
        owner: ref.owner,
        repo: ref.repo,
        organization: FORK_ORG,
        name: input.repoName,
      });
      const data = response.data as { html_url?: string } | undefined;
      return {
        repoUrl:
          data?.html_url ?? `https://github.com/${FORK_ORG}/${input.repoName}`,
      };
    },
    async publish(input: PublishInput): Promise<PublishResult> {
      const result = await github.shell(".", [
        "npm",
        "publish",
        "--access",
        "public",
      ]);
      if (result.code !== 0) {
        throw new Error(
          `npm publish failed for ${input.forkNpmName}: ${result.output}`,
        );
      }
      return { name: input.forkNpmName, version: "0.0.0" };
    },
  };
}

/**
 * Build the PRODUCTION adapter bundle: the real GitHub/git/process adapter, the
 * supplied metadata source, and a takeover boundary derived from the GitHub
 * adapter + token. Tests construct {@link MaintenanceAdapters} directly with
 * fakes instead of calling this.
 */
export function createProductionAdapters(
  config: ProductionAdaptersConfig,
): MaintenanceAdapters {
  const github = createGitHubAdapter({ token: config.token });
  return {
    github,
    metadata: config.metadata,
    takeover: createTakeoverAdapter(config.token, github),
    // The Treaty step shells its transforms + treaty-packagr through the SAME
    // process boundary the github adapter owns; only wired when enabled.
    ...(config.enableTreaty === true
      ? { treaty: createTreatyMigrationStep(github.shell) }
      : {}),
  };
}
