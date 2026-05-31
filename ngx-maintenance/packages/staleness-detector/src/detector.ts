import { discoverStale, evaluateCandidate } from "./staleness.js";
import type {
  AngularMajor,
  DiscoveryCandidate,
  MetadataSource,
  StaleFinding,
} from "./types.js";

/**
 * Drives the pure detection predicates through an injected {@link MetadataSource}
 * (the github-adapter in production, a fake in tests). The detector itself adds
 * NO logic beyond reading the port and delegating to the pure functions; this
 * keeps all decisions deterministic and testable without network access.
 */
export class StalenessDetector {
  constructor(private readonly source: MetadataSource) {}

  /**
   * Evaluate a single opted-in library by npm name against the current latest
   * Angular major. Returns `undefined` when the package cannot be resolved.
   */
  async evaluate(
    npmName: string,
    now: number,
    latestAngular?: AngularMajor,
  ): Promise<DiscoveryCandidate | undefined> {
    const metadata = await this.source.fetchMetadata(npmName);
    if (metadata === undefined) {
      return undefined;
    }
    const latest =
      latestAngular ?? (await this.source.fetchLatestAngularMajor());
    return evaluateCandidate(metadata, latest, now);
  }

  /**
   * Scan every candidate the source enumerates and return the stale findings
   * (behind latest AND > 6 months inactive), preserving discovery order.
   */
  async discover(now: number): Promise<readonly StaleFinding[]> {
    const [latest, candidates] = await Promise.all([
      this.source.fetchLatestAngularMajor(),
      this.source.listCandidates(),
    ]);
    return discoverStale(candidates, latest, now);
  }
}
