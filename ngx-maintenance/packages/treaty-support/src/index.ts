/**
 * @ngx-maintenance/treaty-support
 *
 * Optionally migrate a maintained fork to Treaty's enhanced authoring and
 * build/package it with the Treaty compiler / treaty-packagr, so adopted forks
 * can move to Treaty. Supports all Treaty authoring. Deterministic, no AI: the
 * migration is a fixed set of structural transforms plus a packaging invocation.
 */

export type {
  TreatyAuthoringMode,
  TreatySupportPlan,
  TreatyPackageOptions,
} from "./types.js";

export { planTreatyMigration, treatyPackagrArgv } from "./support.js";
