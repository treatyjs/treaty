import {
  buildMigrationPr,
  cloneAndMigrate,
  findOpenMigrationPr,
  openMigrationPr,
  type CloneMigrateResult,
} from "@ngx-maintenance/github-adapter";
import { planMigrationChain } from "@ngx-maintenance/migration-engine";
import {
  describeStale,
  isBehindLatest,
  type RepoMetadata,
  type StaleFinding,
} from "@ngx-maintenance/staleness-detector";
import {
  executeTakeover,
  isUnmaintained,
  type TakeoverDecision,
  type TakeoverRun,
  type TakeoverSignals,
} from "@ngx-maintenance/takeover";
import type { TreatyStepResult } from "@ngx-maintenance/treaty-support";
import type { BotConfig } from "./config.js";
import type { MaintenanceAdapters } from "./adapters.js";

/**
 * An outstanding migration PR the host is tracking for takeover eligibility.
 * The orchestrator does not persist state between cycles; the host supplies the
 * PRs it has open along with their timer/activity signals, and the cycle
 * decides — deterministically — which have crossed the takeover window.
 */
export interface OutstandingPr {
  /** npm package name of the library whose migration PR is outstanding. */
  readonly npmName: string;
  /** Source repository URL. */
  readonly repoUrl: string;
  /** The timer + activity signals the takeover policy consults. */
  readonly signals: TakeoverSignals;
}

/** Everything one maintenance cycle needs beyond the static config + adapters. */
export interface MaintenanceCycleInput {
  /** The evaluation instant (epoch ms). Drives staleness + takeover timers. */
  readonly now: number;
  /**
   * Migration PRs the host currently has open, to be evaluated for the 2-week
   * takeover window this cycle. Empty on a discovery-only cycle.
   */
  readonly outstandingPrs?: readonly OutstandingPr[];
}

/** Why a discovered library was not migrated this cycle. */
export type SkipReason =
  | "not-stale"
  | "empty-plan"
  | "clone-failed"
  | "migration-failed"
  | "treaty-failed"
  | "pr-exists";

/** The per-library outcome of the discover -> migrate -> PR pipeline. */
export interface LibraryOutcome {
  /** npm package name of the evaluated library. */
  readonly npmName: string;
  /** Source repository URL. */
  readonly repoUrl: string;
  /** True when the library was stale (behind latest AND inactive). */
  readonly stale: boolean;
  /** The stale finding (reason + majors behind), when the library was stale. */
  readonly finding: StaleFinding | undefined;
  /** The clone+migrate result, when a migration was attempted. */
  readonly migration: CloneMigrateResult | undefined;
  /** The migration head branch a PR was opened on, when one was opened. */
  readonly prHead: string | undefined;
  /**
   * The optional Treaty migration step's result, present ONLY when the library
   * opted in (its name is in `config.treatyOptIn`) AND a Treaty adapter was
   * injected. `undefined` for every non-opted-in library — the unchanged path.
   */
  readonly treaty: TreatyStepResult | undefined;
  /**
   * `"pr-opened"` when a new migration PR was created; otherwise the reason the
   * library was skipped.
   */
  readonly result: "pr-opened" | SkipReason;
}

/** The takeover outcome for one outstanding PR this cycle. */
export interface TakeoverOutcome {
  /** npm package name of the library the PR targeted. */
  readonly npmName: string;
  /** Source repository URL. */
  readonly repoUrl: string;
  /** The policy decision + (if it fired) the executed fork/publish. */
  readonly run: TakeoverRun;
}

/** The full, deterministic result of one maintenance cycle. */
export interface MaintenanceCycleResult {
  /** The evaluation instant the cycle ran at. */
  readonly now: number;
  /** Per-library discover -> migrate -> PR outcomes, in discovery order. */
  readonly libraries: readonly LibraryOutcome[];
  /** Per-PR takeover outcomes, in the order the PRs were supplied. */
  readonly takeovers: readonly TakeoverOutcome[];
  /** Count of new migration PRs opened this cycle. */
  readonly prsOpened: number;
  /** Count of forks the takeover step created this cycle. */
  readonly takeoversExecuted: number;
}

/**
 * Run ONE deterministic maintenance cycle, wiring the four core packages
 * through the injected {@link MaintenanceAdapters} boundary. No real network,
 * git, process or filesystem call happens in the logic — they all go through
 * the adapters, so the whole flow is fake-testable.
 *
 * The flow, per the plan:
 *  1. DISCOVER candidate libraries (metadata source) and run the staleness
 *     detector against `now` + the latest Angular major.
 *  2. For each STALE library: plan the migration chain, clone it into a fresh
 *     workdir and run the chain there, then IDEMPOTENTLY open a migration PR
 *     (skipping when one is already open on the migration head branch). A
 *     clone/migration failure stops short of opening a PR — a human reviews.
 *  3. For each outstanding migration PR unmerged >= the takeover window against
 *     an unmaintained library, invoke the takeover module to plan + create the
 *     `@ngx-maintenance/*` fork repo through the takeover adapter.
 *
 * Every allocated workdir is disposed before returning, even on failure.
 */
export async function runMaintenanceCycle(
  config: BotConfig,
  adapters: MaintenanceAdapters,
  input: MaintenanceCycleInput,
): Promise<MaintenanceCycleResult> {
  const { now } = input;

  // DISCOVER: read every candidate and the latest Angular major once, then run
  // the pure staleness predicates. A library is stale iff it is BOTH behind
  // latest AND inactive for longer than the configured window.
  const [latest, candidates] = await Promise.all([
    adapters.metadata.fetchLatestAngularMajor(),
    adapters.metadata.listCandidates(),
  ]);
  const findingByName = new Map<string, StaleFinding>();
  for (const candidate of candidates) {
    if (isStaleAgainstWindow(candidate, latest, now, config.stalenessWindowMs)) {
      findingByName.set(
        candidate.npmName,
        describeStale(candidate, latest, now),
      );
    }
  }

  const libraries: LibraryOutcome[] = [];
  let prsOpened = 0;

  for (const candidate of candidates) {
    const base = {
      npmName: candidate.npmName,
      repoUrl: candidate.repoUrl,
    };
    const finding = findingByName.get(candidate.npmName);

    // Up-to-date / active library: nothing to do.
    if (finding === undefined) {
      libraries.push({
        ...base,
        stale: false,
        finding: undefined,
        migration: undefined,
        prHead: undefined,
        treaty: undefined,
        result: "not-stale",
      });
      continue;
    }

    const plan = planMigrationChain(
      candidate.currentAngular,
      config.targetAngular,
    );
    // Stale but already at (or past) target with no VE window step: no chain.
    if (plan.steps.length === 0) {
      libraries.push({
        ...base,
        stale: true,
        finding,
        migration: undefined,
        prHead: undefined,
        treaty: undefined,
        result: "empty-plan",
      });
      continue;
    }

    const prSpec = buildMigrationPr(base, plan, config.baseBranch);

    // IDEMPOTENCY: a pure read of open PRs on the head branch. If one already
    // exists, do no work for this library this cycle (no clone, no re-open).
    // eslint-disable-next-line no-await-in-loop
    const existing = await findOpenMigrationPr(adapters.github.octokit, prSpec);
    if (existing !== undefined) {
      libraries.push({
        ...base,
        stale: true,
        finding,
        migration: undefined,
        prHead: prSpec.head,
        treaty: undefined,
        result: "pr-exists",
      });
      continue;
    }

    // Clone into a fresh workdir, run the migration chain there, and — for an
    // opted-in library only — run the OPTIONAL Treaty step on the same workdir
    // before it is disposed. A non-opted-in library never touches Treaty.
    // eslint-disable-next-line no-await-in-loop
    const { migration, treaty } = await runClonedMigration(
      config,
      adapters,
      prSpec,
      candidate.npmName,
    );
    if (!migration.clone.ok) {
      libraries.push({
        ...base,
        stale: true,
        finding,
        migration,
        prHead: undefined,
        treaty: undefined,
        result: "clone-failed",
      });
      continue;
    }
    if (!migration.ok) {
      // A failed step is reported, never papered over by an LLM. No PR opens.
      libraries.push({
        ...base,
        stale: true,
        finding,
        migration,
        prHead: undefined,
        treaty: undefined,
        result: "migration-failed",
      });
      continue;
    }
    if (treaty !== undefined && !treaty.ok) {
      // The opt-in Treaty step failed: report it for human review, no PR opens.
      libraries.push({
        ...base,
        stale: true,
        finding,
        migration,
        prHead: undefined,
        treaty,
        result: "treaty-failed",
      });
      continue;
    }

    // Migration (and any opted-in Treaty step) succeeded: open the PR (the read
    // above proved none was open).
    // eslint-disable-next-line no-await-in-loop
    await openMigrationPr(adapters.github.octokit, prSpec);
    libraries.push({
      ...base,
      stale: true,
      finding,
      migration,
      prHead: prSpec.head,
      treaty,
      result: "pr-opened",
    });
    prsOpened += 1;
  }

  const takeovers: TakeoverOutcome[] = [];
  let takeoversExecuted = 0;
  for (const pr of input.outstandingPrs ?? []) {
    const decision = decideTakeoverWithWindow(
      pr.signals,
      now,
      config.takeoverWindowMs,
    );
    if (!decision.shouldTakeover) {
      takeovers.push({
        npmName: pr.npmName,
        repoUrl: pr.repoUrl,
        run: { decision, spec: undefined, execution: undefined },
      });
      continue;
    }
    // eslint-disable-next-line no-await-in-loop
    const execution = await executeTakeover(
      { npmName: pr.npmName, repoUrl: pr.repoUrl },
      adapters.takeover,
    );
    takeovers.push({
      npmName: pr.npmName,
      repoUrl: pr.repoUrl,
      run: { decision, spec: execution.spec, execution },
    });
    takeoversExecuted += 1;
  }

  return { now, libraries, takeovers, prsOpened, takeoversExecuted };
}

/** Normalise an epoch-ms-or-`Date` timestamp to epoch milliseconds. */
function toMs(t: number | Date): number {
  return typeof t === "number" ? t : t.getTime();
}

/** Resolve when a PR opened from the explicit or legacy signal field. */
function prOpenedMs(signals: TakeoverSignals): number {
  if (signals.prOpenedAt !== undefined) return toMs(signals.prOpenedAt);
  if (signals.prOpenedMs !== undefined) return signals.prOpenedMs;
  throw new TypeError("takeover requires `prOpenedAt` on the PR signals");
}

/**
 * Decide takeover against the CONFIGURED window rather than only the takeover
 * package's default 2-week constant. The PR must be unmerged AND open for >=
 * `windowMs` (inclusive boundary, so a 15-day PR fires under a 14-day window
 * and a 13-day one does not) AND the library clearly unmaintained. Delegates
 * the abandonment half to the package's {@link isUnmaintained}.
 */
function decideTakeoverWithWindow(
  signals: TakeoverSignals,
  now: number,
  windowMs: number,
): TakeoverDecision {
  const windowElapsed =
    !signals.prMerged && now - prOpenedMs(signals) >= windowMs;
  const unmaintained = isUnmaintained(signals, now);
  return {
    windowElapsed,
    unmaintained,
    shouldTakeover: windowElapsed && unmaintained,
  };
}

/**
 * Whether a candidate is stale against the CONFIGURED window: behind the latest
 * Angular major AND idle for longer than `windowMs`. Uses the staleness
 * detector's `isBehindLatest` predicate and an explicit window comparison so the
 * operator-tunable threshold (not just the package default 6 months) is honored.
 * The boundary is exclusive: at exactly the window the library is still active.
 */
function isStaleAgainstWindow(
  metadata: RepoMetadata,
  latest: number,
  now: number,
  windowMs: number,
): boolean {
  return (
    isBehindLatest(metadata.currentAngular, latest) &&
    now - metadata.lastCommitMs > windowMs
  );
}

/** The combined clone+migrate and (opt-in) Treaty outcome for one library. */
interface ClonedMigrationOutcome {
  /** The clone + Angular migration-chain result. */
  readonly migration: CloneMigrateResult;
  /**
   * The Treaty step result, present ONLY when the library opted in AND a Treaty
   * adapter was injected AND the Angular migration succeeded (so a workdir to
   * run it against exists). `undefined` otherwise.
   */
  readonly treaty: TreatyStepResult | undefined;
}

/**
 * Decide whether the OPTIONAL Treaty step runs for a library: it runs iff the
 * library opted in (its name is in `config.treatyOptIn`) AND a Treaty adapter
 * was injected. Pure — a non-opted-in library, or a deployment with no Treaty
 * boundary, returns `undefined` and the flow is unchanged.
 */
function treatyStepFor(
  config: BotConfig,
  adapters: MaintenanceAdapters,
  npmName: string,
): MaintenanceAdapters["treaty"] {
  if (adapters.treaty === undefined) return undefined;
  return config.treatyOptIn.includes(npmName) ? adapters.treaty : undefined;
}

/**
 * Clone the library into a fresh workdir and run its migration chain, then —
 * for an opted-in library whose migration succeeded — run the additional Treaty
 * step on that SAME workdir, disposing the workdir afterward in all cases.
 * Isolated so the cycle's control flow stays readable.
 */
async function runClonedMigration(
  config: BotConfig,
  adapters: MaintenanceAdapters,
  prSpec: ReturnType<typeof buildMigrationPr>,
  slug: string,
): Promise<ClonedMigrationOutcome> {
  const dir = await adapters.github.workdirs.allocate(slug);
  try {
    const migration = await cloneAndMigrate(
      adapters.github.shell,
      prSpec.repoUrl,
      dir,
      prSpec.plan,
      config.cloneDepth !== undefined ? { ref: prSpec.base, depth: config.cloneDepth } : { ref: prSpec.base },
    );

    // The Treaty step only makes sense on a successfully migrated checkout.
    const step = treatyStepFor(config, adapters, prSpec.npmName);
    if (step === undefined || !migration.clone.ok || !migration.ok) {
      return { migration, treaty: undefined };
    }

    const treaty = await step.migrate({
      npmName: prSpec.npmName,
      workdir: dir,
      mode: config.treatyMode,
      outDir: "dist",
    });
    return { migration, treaty };
  } finally {
    await adapters.github.workdirs.dispose(dir);
  }
}
