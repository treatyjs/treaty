/**
 * Ambient structural declarations for external infrastructure dependencies.
 *
 * These deps (octokit, @angular/cli) are NOT installed in this Turborepo: they are
 * supplied out-of-band at deploy time. We declare only the surface we structurally
 * depend on so `tsgo --noEmit` typechecks without an install step. The runtime app
 * still installs the real packages; these shapes intentionally describe a subset.
 */

declare module "octokit" {
  export interface OctokitRequestParams {
    [key: string]: unknown;
  }

  export interface OctokitResponse<T = unknown> {
    status: number;
    data: T;
  }

  export interface OctokitRest {
    pulls: {
      create(params: OctokitRequestParams): Promise<OctokitResponse>;
      merge(params: OctokitRequestParams): Promise<OctokitResponse>;
      list(params: OctokitRequestParams): Promise<OctokitResponse<unknown[]>>;
    };
    issues: {
      create(params: OctokitRequestParams): Promise<OctokitResponse>;
      createComment(params: OctokitRequestParams): Promise<OctokitResponse>;
    };
    repos: {
      get(params: OctokitRequestParams): Promise<OctokitResponse>;
      listCommits(
        params: OctokitRequestParams,
      ): Promise<OctokitResponse<unknown[]>>;
      createFork(params: OctokitRequestParams): Promise<OctokitResponse>;
    };
  }

  export class Octokit {
    constructor(options?: { auth?: string; [key: string]: unknown });
    rest: OctokitRest;
    request(
      route: string,
      params?: OctokitRequestParams,
    ): Promise<OctokitResponse>;
  }
}

declare module "@octokit/webhooks-types" {
  export interface InstallationEvent {
    action: string;
    installation: { id: number; account: { login: string } };
    repositories?: Array<{ full_name: string }>;
  }

  export interface PushEvent {
    ref: string;
    repository: { full_name: string; default_branch: string };
  }

  export interface ReleaseEvent {
    action: string;
    release: { tag_name: string };
    repository: { full_name: string };
  }
}
