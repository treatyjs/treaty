import { decideTakeover } from "./policy.js";
import { planTakeover } from "./plan.js";
import type {
  NewRepoSpec,
  TakeoverDecision,
  TakeoverLib,
  TakeoverSignals,
  TakeoverSpec,
  Timestamp,
} from "./types.js";

/**
 * The out-of-band side-effect boundary. The takeover core is pure: it decides
 * (policy) and emits a {@link TakeoverSpec} (plan). The ACTUAL fork, npm publish
 * and standalone-repo creation are real GitHub / git / npm calls and live behind
 * this injected interface, so the orchestration logic stays deterministic and is
 * unit-tested with a fake. NO AI, and no network in the core.
 *
 * Every method takes the relevant slice of the already-computed spec; an adapter
 * implementation performs the effect and returns where the effect landed.
 */
export interface GithubAdapter {
  /**
   * Fork the source repository into the `@ngx-maintenance` org. Returns the URL
   * of the created fork. Implementations call the GitHub fork API + git.
   */
  fork(input: ForkInput): Promise<ForkResult>;
  /**
   * Create the NEW standalone repository each takeover becomes. Returns the URL
   * of the created repo. Implementations call the GitHub repo-creation API.
   */
  createRepo(spec: NewRepoSpec): Promise<CreateRepoResult>;
  /**
   * Publish the scoped compatibility-only fork to npm. Returns the published
   * name + version. Implementations call `npm publish` against the staged tree.
   */
  publish(input: PublishInput): Promise<PublishResult>;
}

/** The inputs the adapter needs to fork the source repository. */
export interface ForkInput {
  /** The source repository URL being forked. */
  readonly sourceRepoUrl: string;
  /** The name the standalone fork repository should take. */
  readonly repoName: string;
}

/** Where a fork landed. */
export interface ForkResult {
  /** URL of the created fork. */
  readonly repoUrl: string;
}

/** Where a created standalone repository landed. */
export interface CreateRepoResult {
  /** URL of the created repository. */
  readonly repoUrl: string;
}

/** The inputs the adapter needs to publish the scoped fork to npm. */
export interface PublishInput {
  /** The scoped fork name, e.g. `@ngx-maintenance/widget`. */
  readonly forkNpmName: string;
  /** The compatibility-only warning banner (becomes the npm description). */
  readonly warningBanner: string;
}

/** What an npm publish produced. */
export interface PublishResult {
  /** The published package name. */
  readonly name: string;
  /** The published version. */
  readonly version: string;
}

/** The outcome of executing a takeover through the adapter. */
export interface TakeoverExecution {
  /** The deterministic spec that drove the side effects. */
  readonly spec: TakeoverSpec;
  /** Where the fork landed. */
  readonly fork: ForkResult;
  /** Where the new standalone repo landed. */
  readonly repo: CreateRepoResult;
  /** What the npm publish produced. */
  readonly publish: PublishResult;
}

/** The outcome of {@link runTakeover}: the decision plus, if fired, execution. */
export interface TakeoverRun {
  /** The fully-explained policy decision. */
  readonly decision: TakeoverDecision;
  /**
   * The spec, present whenever the takeover fired (it is pure data, so it is
   * emitted even before side effects). `undefined` when the policy did not fire.
   */
  readonly spec: TakeoverSpec | undefined;
  /** The executed side effects, present only when the takeover fired. */
  readonly execution: TakeoverExecution | undefined;
}

/**
 * Execute a takeover against an injected {@link GithubAdapter}, in the fixed,
 * deterministic order: create the standalone repo, fork the source into it, then
 * publish the scoped compatibility-only fork. The spec is computed purely by
 * {@link planTakeover}; the adapter performs the real (out-of-band) effects.
 *
 * This does NOT evaluate policy — the caller has already decided to take over.
 * Use {@link runTakeover} to gate the execution on the policy decision.
 */
export async function executeTakeover(
  lib: TakeoverLib,
  adapter: GithubAdapter,
): Promise<TakeoverExecution> {
  const spec = planTakeover(lib);
  const repo = await adapter.createRepo(spec.newRepo);
  const fork = await adapter.fork({
    sourceRepoUrl: spec.sourceRepoUrl,
    repoName: spec.newRepo.name,
  });
  const publish = await adapter.publish({
    forkNpmName: spec.forkNpmName,
    warningBanner: spec.warningBanner,
  });
  return { spec, fork, repo, publish };
}

/**
 * Evaluate the takeover policy for `lib` and, only if it fires, execute the
 * takeover through the adapter. The decision is a pure function of `signals`
 * (and `now`); the side effects are delegated to the injected adapter. When the
 * policy does not fire, no adapter method is called and `execution` is
 * `undefined`. NO AI.
 */
export async function runTakeover(
  lib: TakeoverLib,
  signals: TakeoverSignals,
  adapter: GithubAdapter,
  now?: Timestamp,
): Promise<TakeoverRun> {
  const decision = decideTakeover(signals, now);
  if (!decision.shouldTakeover) {
    return { decision, spec: undefined, execution: undefined };
  }
  const execution = await executeTakeover(lib, adapter);
  return { decision, spec: execution.spec, execution };
}
