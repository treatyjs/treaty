import type {
  InstallationEvent,
  PushEvent,
  ReleaseEvent,
} from "@octokit/webhooks-types";
import type { BotConfig, BotContext } from "./context.js";
import { createBotContext } from "./context.js";
import type { RegistryManifest } from "@ngx-maintenance/registry";
import {
  onAngularRelease,
  onInstallation,
  onPush,
  type HandlerResult,
  type ReleaseResult,
} from "./handlers.js";
import {
  runScheduledScan,
  type ScanResult,
  type ScheduleTick,
} from "./scheduler.js";

/**
 * The thin webhook application shell. Actual server transport (HTTP listener,
 * signature verification, secret loading) is supplied out-of-band; this shell
 * exposes the typed event dispatch + scheduler entry points that the transport
 * forwards to. Each method rebuilds context from the freshest manifest so the
 * handlers stay pure with respect to the manifest snapshot they receive.
 */
export interface BotApp {
  readonly context: BotContext;
  /** Dispatch an installation webhook. */
  onInstallation(event: InstallationEvent): HandlerResult;
  /** Dispatch an `@angular/core` release webhook. */
  onRelease(event: ReleaseEvent): Promise<ReleaseResult>;
  /** Dispatch a push webhook. */
  onPush(event: PushEvent): HandlerResult;
  /** Run a scheduled scan tick (cron-driven). */
  onScheduledTick(tick: ScheduleTick): Promise<ScanResult>;
}

/**
 * Build the bot application from static config and the current manifest. The
 * returned shell holds a single {@link BotContext}; a real deployment would
 * refresh the manifest between events, which callers can do by constructing a
 * new app with the updated manifest.
 */
export function createApp(
  config: BotConfig,
  manifest: RegistryManifest,
): BotApp {
  const context = createBotContext(config, manifest);
  return {
    context,
    onInstallation: (event) => onInstallation(context, event),
    onRelease: (event) => onAngularRelease(context, event),
    onPush: (event) => onPush(context, event),
    onScheduledTick: (tick) => runScheduledScan(context, tick),
  };
}
