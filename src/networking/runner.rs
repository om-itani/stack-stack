//! Command execution behind a trait, so apply logic can be tested without a
//! Linux host or root privileges.

use std::time::Duration;

use crate::proc::{self, Output};

/// Every external command a network apply or probe runs goes through this.
pub trait Runner: Send + Sync {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, String>;

    /// Convenience for literal argument lists.
    fn call(&self, program: &str, args: &[&str]) -> Result<Output, String> {
        let owned: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        self.run(program, &owned)
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
        // `systemctl restart` on a slow box is the longest thing we run here.
        Self::new(Duration::from_secs(30))
    }
}

impl Runner for SystemRunner {
    fn run(&self, program: &str, args: &[String]) -> Result<Output, String> {
        proc::run(program, args, None, self.timeout)
            .map_err(|err| format!("cannot run {program}: {err}"))
    }
}

#[cfg(test)]
pub mod fake {
    //! Scripted runner used by the apply tests.

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

    #[derive(Default)]
    pub struct FakeRunner {
        calls: Mutex<Vec<(String, Vec<String>)>>,
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
