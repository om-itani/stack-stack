//! Command execution behind a trait, shared by every subsystem that shells out.
//!
//! Both the router and the virtualization layer drive external tools (`nft`,
//! `qemu-system-x86_64`, `lxc-create`, …). Putting the seam here means apply and
//! lifecycle logic can be tested without a Linux host, root privileges or KVM,
//! and there is exactly one place that spawns a process with a wall-clock
//! deadline.

use std::time::Duration;

use crate::proc::{self, Output};

/// Every external command an apply, probe or lifecycle step runs goes through
/// this. Implementations must be cheap to call from a blocking context and must
/// never panic: a missing binary is a normal, reported outcome.
pub trait Runner: Send + Sync {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, String>;

    /// Convenience for literal argument lists.
    fn call(&self, program: &str, args: &[&str]) -> Result<Output, String> {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        self.run(program, &owned)
    }

    /// Run with an explicit deadline. Implementations that cannot honour it fall
    /// back to their default timeout rather than failing.
    fn run_timeout(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<Output, String> {
        let _ = timeout;
        self.run(program, args)
    }

    /// Run with bytes on stdin, for secrets that must not appear in argv.
    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        input: &[u8],
    ) -> Result<Output, String> {
        let _ = (timeout, input);
        self.run(program, args)
    }

    /// Run with extra environment variables, for secrets handed to restic.
    /// Implementations that cannot set the environment ignore `env`.
    fn run_env(
        &self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<Output, String> {
        let _ = env;
        self.run_timeout(program, args, timeout)
    }

    /// True when the program exists and ran; used for capability probing.
    fn available(&self, program: &str, probe: &[&str]) -> bool {
        self.call(program, probe)
            .map(|output| !output.timed_out)
            .unwrap_or(false)
    }
}

/// Real runner: spawns the process with a hard deadline.
pub struct SystemRunner {
    timeout: Duration,
}

impl SystemRunner {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for SystemRunner {
    fn default() -> Self {
        // `systemctl restart` on a slow box is the longest thing the router
        // runs; virtualization lifecycle steps pass their own, longer deadline.
        Self::new(Duration::from_secs(30))
    }
}

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, String> {
        proc::run(program, args, None, self.timeout)
            .map_err(|err| format!("cannot run {program}: {err}"))
    }

    fn run_timeout(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
    ) -> Result<Output, String> {
        proc::run(program, args, None, timeout)
            .map_err(|err| format!("cannot run {program}: {err}"))
    }

    fn run_with_input(
        &self,
        program: &str,
        args: &[String],
        timeout: Duration,
        input: &[u8],
    ) -> Result<Output, String> {
        proc::run_with_input(program, args, None, timeout, input)
            .map_err(|err| format!("cannot run {program}: {err}"))
    }

    fn run_env(
        &self,
        program: &str,
        args: &[String],
        env: &[(String, String)],
        timeout: Duration,
    ) -> Result<Output, String> {
        proc::run_env(program, args, env, timeout)
            .map_err(|err| format!("cannot run {program}: {err}"))
    }
}

#[cfg(test)]
pub mod fake {
    //! Scripted runner used by apply/lifecycle tests.

    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;

    /// A scripted response. `contains` narrows it to calls whose joined
    /// arguments include that substring, which is how one program can answer
    /// differently for `iw list` and `iw dev wlan0 info`.
    struct Rule {
        program: String,
        contains: Option<String>,
        output: Output,
    }

    /// One recorded `run_env` call: program, arguments and the environment.
    type EnvCall = (String, Vec<String>, Vec<(String, String)>);

    #[derive(Default)]
    pub struct FakeRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
        env_calls: Mutex<Vec<EnvCall>>,
        generic: HashMap<String, Output>,
        conditional: Vec<Rule>,
        missing: Vec<String>,
    }

    impl FakeRunner {
        pub fn new() -> Self {
            Self::default()
        }

        /// Program fails to spawn, as if the binary is not installed.
        pub fn missing(mut self, program: &str) -> Self {
            self.missing.push(program.to_string());
            self
        }

        /// Default result for every invocation of a program.
        pub fn respond(
            mut self,
            program: &str,
            code: Option<i32>,
            stdout: &str,
            stderr: &str,
        ) -> Self {
            self.generic
                .insert(program.to_string(), output(code, stdout, stderr));
            self
        }

        /// Result for invocations of a program whose arguments contain `needle`.
        /// Checked before the generic response.
        pub fn respond_when(
            mut self,
            program: &str,
            needle: &str,
            code: Option<i32>,
            stdout: &str,
            stderr: &str,
        ) -> Self {
            self.conditional.push(Rule {
                program: program.to_string(),
                contains: Some(needle.to_string()),
                output: output(code, stdout, stderr),
            });
            self
        }

        pub fn calls(&self) -> Vec<(String, Vec<String>)> {
            self.calls.lock().expect("calls lock").clone()
        }

        pub fn ran(&self, program: &str) -> bool {
            self.calls().iter().any(|(name, _)| name == program)
        }

        pub fn ran_with(&self, program: &str, args: &[&str]) -> bool {
            self.calls().iter().any(|(name, call_args)| {
                name == program
                    && call_args
                        .iter()
                        .map(String::as_str)
                        .eq(args.iter().copied())
            })
        }

        /// Every environment variable set on an `run_env` call for `program`.
        pub fn env_vars(&self, program: &str) -> Vec<(String, String)> {
            self.env_calls
                .lock()
                .expect("env lock")
                .iter()
                .filter(|(name, _, _)| name == program)
                .flat_map(|(_, _, env)| env.clone())
                .collect()
        }
    }

    impl Runner for FakeRunner {
        fn run(&self, program: &str, args: &[String]) -> Result<Output, String> {
            self.calls
                .lock()
                .expect("calls lock")
                .push((program.to_string(), args.to_vec()));

            if self.missing.iter().any(|missing| missing == program) {
                return Err(format!("cannot run {program}: not found"));
            }

            let joined = args.join(" ");
            if let Some(rule) = self.conditional.iter().find(|rule| {
                rule.program == program
                    && rule
                        .contains
                        .as_ref()
                        .is_some_and(|needle| joined.contains(needle.as_str()))
            }) {
                return Ok(rule.output.clone());
            }

            Ok(self
                .generic
                .get(program)
                .cloned()
                .unwrap_or_else(|| output(Some(0), "", "")))
        }

        fn run_env(
            &self,
            program: &str,
            args: &[String],
            env: &[(String, String)],
            _timeout: Duration,
        ) -> Result<Output, String> {
            self.env_calls.lock().expect("env lock").push((
                program.to_string(),
                args.to_vec(),
                env.to_vec(),
            ));
            self.run(program, args)
        }
    }

    fn output(code: Option<i32>, stdout: &str, stderr: &str) -> Output {
        Output {
            code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            timed_out: false,
        }
    }
}
