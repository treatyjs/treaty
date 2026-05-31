import {
  advanceState,
  tick,
  type MaintenanceCycleResult,
  type SchedulerConfig,
  type SchedulerState,
} from "@ngx-maintenance/orchestrator";
import type { Runner } from "./runner.js";

/** The outcome of a single host-driven poll. */
export interface PollOutcome {
  /** Whether a cycle was due (and therefore ran) at `now`. */
  readonly ran: boolean;
  /** The cycle result, present only when a cycle ran. */
  readonly result: MaintenanceCycleResult | undefined;
  /** The scheduler state to carry into the next poll. */
  readonly state: SchedulerState;
  /** Ms until the next cycle is due (0 when one just ran). */
  readonly nextDueInMs: number;
}

/**
 * Perform ONE poll at the host-supplied `now`: ask the pure {@link tick}
 * whether a cycle is due, run it through the {@link Runner} if so, and return
 * the advanced scheduler state. There are NO real timers here — the host cron
 * calls this on its own cadence and threads the returned `state` back in. This
 * keeps the entire scheduling loop deterministic and unit-testable.
 */
export async function poll(
  runner: Runner,
  scheduler: SchedulerConfig,
  state: SchedulerState,
  now: number,
): Promise<PollOutcome> {
  const decision = tick(scheduler, state, now);
  if (!decision.due || decision.input === undefined) {
    return {
      ran: false,
      result: undefined,
      state,
      nextDueInMs: decision.nextDueInMs,
    };
  }
  const result = await runner.run(decision.input);
  return {
    ran: true,
    result,
    state: advanceState(state, now),
    nextDueInMs: 0,
  };
}
