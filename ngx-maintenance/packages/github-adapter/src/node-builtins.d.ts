/**
 * Minimal ambient declarations for the Node.js built-in modules this package
 * uses for the process (`child_process`) boundary.
 *
 * The ngx-maintenance Turborepo does not install `@types/node` per package, and
 * consumers that typecheck through our source (via tsconfig `paths`) must not be
 * forced to add `"types": ["node"]`. Declaring only the surface we use keeps the
 * adapter self-contained: `tsgo --noEmit` resolves these modules without any
 * install step, and the real Node runtime supplies the implementations.
 */

declare module "node:child_process" {
  /** The structural subset of a stream chunk we consume: stringifiable. */
  interface Chunk {
    toString(): string;
  }
  interface ChildStream {
    on(event: "data", listener: (chunk: Chunk) => void): void;
  }
  interface ChildProcess {
    readonly stdout: ChildStream;
    readonly stderr: ChildStream;
    on(event: "error", listener: (error: Error) => void): void;
    on(event: "close", listener: (code: number | null) => void): void;
  }
  export function spawn(
    command: string,
    args: readonly string[],
    options: { cwd: string },
  ): ChildProcess;
}
