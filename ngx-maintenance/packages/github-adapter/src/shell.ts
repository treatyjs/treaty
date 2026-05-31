import { spawn } from "node:child_process";

/** The result of one shelled-out command. */
export interface CommandResult {
  /** Process exit code; 0 means success. */
  readonly code: number;
  /** Combined stdout + stderr, in arrival order. */
  readonly output: string;
}

/**
 * A structural shell: runs one argv in `repoDir` and resolves with its exit
 * code + combined output. The concrete implementation (spawning `ng`, `npm`,
 * git, etc.) is injected so all orchestration stays pure and unit-testable.
 *
 * This is the SINGLE process boundary for the monorepo. The migration engine
 * shells `ng update` / `npm` through this type; the clone operations shell
 * `git` through it. NO AI is involved — a shell is a pure exit-code + output
 * function from the caller's point of view.
 */
export type Shell = (
  repoDir: string,
  argv: readonly string[],
) => Promise<CommandResult>;

/**
 * Production shell: spawn `argv[0]` with the remaining args in `repoDir`,
 * capturing stdout + stderr into a single combined string and resolving with
 * the exit code. A spawn error (e.g. command not found) resolves with code 127
 * rather than rejecting, so callers always get a structured {@link
 * CommandResult} to log. `shell: false` (the default) avoids shell injection;
 * the argv is passed verbatim to the OS.
 */
export function createNodeShell(): Shell {
  return (repoDir, argv) =>
    new Promise<CommandResult>((resolve) => {
      const [command, ...args] = argv;
      if (command === undefined) {
        resolve({ code: 0, output: "" });
        return;
      }
      let output = "";
      const child = spawn(command, args, { cwd: repoDir });
      child.stdout.on("data", (chunk) => {
        output += chunk.toString();
      });
      child.stderr.on("data", (chunk) => {
        output += chunk.toString();
      });
      child.on("error", (error: Error) => {
        resolve({ code: 127, output: `${output}${error.message}` });
      });
      child.on("close", (code: number | null) => {
        resolve({ code: code ?? 0, output });
      });
    });
}

/** One recorded invocation made against a {@link FakeShell}. */
export interface ShellInvocation {
  readonly repoDir: string;
  readonly argv: readonly string[];
}

/**
 * A deterministic, recording {@link Shell} for tests. It never spawns a
 * process: it records each invocation and returns a pre-programmed result. A
 * `responder` maps an argv to a {@link CommandResult}; when it returns
 * `undefined` (or is omitted) the default success result is used. This lets a
 * test assert exactly which commands the orchestration shelled, in order, and
 * inject failures for a chosen step — all without touching a real process.
 */
export interface FakeShell {
  readonly shell: Shell;
  /** Every invocation, in call order. */
  readonly calls: readonly ShellInvocation[];
}

/** Build a recording fake shell with an optional per-argv responder. */
export function createFakeShell(
  responder?: (invocation: ShellInvocation) => CommandResult | undefined,
): FakeShell {
  const calls: ShellInvocation[] = [];
  const shell: Shell = (repoDir, argv) => {
    const invocation: ShellInvocation = { repoDir, argv };
    calls.push(invocation);
    const result = responder?.(invocation);
    return Promise.resolve(result ?? { code: 0, output: "" });
  };
  return { shell, calls };
}
