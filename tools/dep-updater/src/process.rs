//! A thin, testable process runner.
//!
//! The orchestration shells out to `cargo`, the oracle harness, `oxlint`, etc.
//! to obtain the green/red signal. Wrapping that behind the [`CommandRunner`]
//! trait keeps the orchestration logic pure and lets tests inject a recorded
//! runner instead of spawning real processes — so a dry run never mutates the
//! repo or hits the network.

use std::process::Command;

/// The captured result of running one command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// Process exit code (`-1` if the process was terminated by a signal).
    pub exit_code: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

impl CommandOutcome {
    /// Whether the command exited successfully (exit code zero).
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }

    /// The trailing `max_chars` of stderr, for compact failure reports.
    pub fn stderr_tail(&self, max_chars: usize) -> String {
        let s = self.stderr.trim_end();
        if s.len() <= max_chars {
            return s.to_string();
        }
        let start = s.len() - max_chars;
        // Snap to a char boundary so we never slice mid-UTF-8.
        let start = (start..=s.len())
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(s.len());
        s[start..].to_string()
    }
}

/// Abstraction over running a command in a working directory.
///
/// Implemented by [`RealCommandRunner`] for production and by recorded fakes in
/// tests, enabling deterministic dry runs.
pub trait CommandRunner {
    /// Run `program` with `args` in `cwd`, capturing stdout/stderr.
    fn run(&self, program: &str, args: &[&str], cwd: &str) -> std::io::Result<CommandOutcome>;
}

/// Runs commands as real OS processes.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealCommandRunner;

impl CommandRunner for RealCommandRunner {
    fn run(&self, program: &str, args: &[&str], cwd: &str) -> std::io::Result<CommandOutcome> {
        let output = Command::new(program).args(args).current_dir(cwd).output()?;
        Ok(CommandOutcome {
            exit_code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// A runner that returns canned outcomes and records the calls it received.
    struct RecordingRunner {
        outcome: CommandOutcome,
        calls: RefCell<Vec<String>>,
    }

    impl CommandRunner for RecordingRunner {
        fn run(&self, program: &str, args: &[&str], cwd: &str) -> std::io::Result<CommandOutcome> {
            self.calls
                .borrow_mut()
                .push(format!("{program} {} @ {cwd}", args.join(" ")));
            Ok(self.outcome.clone())
        }
    }

    #[test]
    fn fake_runner_records_calls_without_spawning() {
        let runner = RecordingRunner {
            outcome: CommandOutcome { exit_code: 0, stdout: "ok".into(), stderr: String::new() },
            calls: RefCell::new(Vec::new()),
        };
        let out = runner.run("cargo", &["build", "--workspace"], "/repo").unwrap();
        assert!(out.success());
        assert_eq!(runner.calls.borrow().as_slice(), &["cargo build --workspace @ /repo".to_string()]);
    }

    #[test]
    fn stderr_tail_truncates_on_char_boundary() {
        let out = CommandOutcome { exit_code: 1, stdout: String::new(), stderr: "héllo world".into() };
        let tail = out.stderr_tail(5);
        assert!(tail.len() <= 6); // <= max_chars + the snapped boundary
        assert!(out.stderr.ends_with(&tail));
    }

    #[test]
    fn real_runner_executes_a_process() {
        // Exercise the production path with a portable no-op-ish command.
        let runner = RealCommandRunner;
        let prog = if cfg!(windows) { "cmd" } else { "sh" };
        let args: &[&str] = if cfg!(windows) {
            &["/C", "exit", "0"]
        } else {
            &["-c", "exit 0"]
        };
        let out = runner.run(prog, args, ".").unwrap();
        assert!(out.success());
    }
}
