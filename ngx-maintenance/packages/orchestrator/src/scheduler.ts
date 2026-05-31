import type { OutstandingPr } from "./cycle.js";

/** One day in milliseconds. */
const DAY_MS = 1000 * 60 * 60 * 24;

/** The state a scheduler carries between ticks (the host persists this). */
export interface SchedulerState {
  /** Epoch ms the last cycle ran at, or `undefined` if it never has. */
  readonly lastRunMs: number | undefined;
  /** Outstanding migration PRs the host is tracking for takeover timing. */
  readonly outstandingPrs: readonly OutstandingPr[];
}

/** Static scheduling policy: how often a maintenance cycle should run. */
export interface SchedulerConfig {
  /**
   * Minimum interval between cycles in ms. A tick before this has elapsed since
   * the last run is a no-op. Defaults to one day.
   */
  readonly intervalMs: number;
}

/** Resolve a {@link SchedulerConfig} from a partial input. */
export function resolveSchedulerConfig(
  input: Partial<SchedulerConfig> = {},
): SchedulerConfig {
  return { intervalMs: input.intervalMs ?? DAY_MS };
}

/** The decision a {@link tick} produces. */
export interface TickDecision {
  /** True when a maintenance cycle should run at `now`. */
  readonly due: boolean;
  /** The instant the tick evaluated. */
  readonly now: number;
  /** Ms until the next cycle is due (0 when due now). */
  readonly nextDueInMs: number;
  /**
   * The cycle input to pass to `runMaintenanceCycle` when `due` — carries `now`
   * and the outstanding PRs to evaluate for takeover. `undefined` when not due.
   */
  readonly input:
    | { readonly now: number; readonly outstandingPrs: readonly OutstandingPr[] }
    | undefined;
}

/**
 * A PURE scheduler tick: given the static policy, the carried state and the
 * current instant `now`, decide whether a maintenance cycle is due — with NO
 * real timers. The host cron calls this on its own cadence; the orchestrator
 * stays deterministic and unit-testable. A cycle is due when no cycle has run
 * yet, or the configured interval has elapsed since the last run.
 *
 * After running the returned `input` through `runMaintenanceCycle`, the host
 * advances state via {@link advanceState} with the same `now`.
 */
export function tick(
  config: SchedulerConfig,
  state: SchedulerState,
  now: number,
): TickDecision {
  if (state.lastRunMs === undefined) {
    return {
      due: true,
      now,
      nextDueInMs: 0,
      input: { now, outstandingPrs: state.outstandingPrs },
    };
  }
  const elapsed = now - state.lastRunMs;
  if (elapsed >= config.intervalMs) {
    return {
      due: true,
      now,
      nextDueInMs: 0,
      input: { now, outstandingPrs: state.outstandingPrs },
    };
  }
  return {
    due: false,
    now,
    nextDueInMs: config.intervalMs - elapsed,
    input: undefined,
  };
}

/**
 * Advance the scheduler state after a cycle ran at `now`, optionally replacing
 * the tracked outstanding PRs. Pure — returns a new state, never mutates.
 */
export function advanceState(
  state: SchedulerState,
  now: number,
  outstandingPrs: readonly OutstandingPr[] = state.outstandingPrs,
): SchedulerState {
  return { lastRunMs: now, outstandingPrs };
}

/** Build the initial scheduler state (no cycle has run yet). */
export function initialState(
  outstandingPrs: readonly OutstandingPr[] = [],
): SchedulerState {
  return { lastRunMs: undefined, outstandingPrs };
}
