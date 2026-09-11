//! Local quality-gate runner.
//!
//! `tools gates` is the rust-analyzer-style replacement for the retired shell
//! aggregator: every gate is a spawned cargo/npm invocation defined here in
//! Rust, so the command surface stays linted, testable, and cross-platform.
//! The groups mirror the CI jobs; contributors run the same commands CI runs.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::cli::CliError;

/// One executable gate: a process to spawn (with optional env overrides) or
/// the in-repository release checks that `tools check release` reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GateAction {
    /// Spawn a process with the repository root as the working directory.
    Command {
        name: String,
        program: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    /// Run [`crate::check::check_release_artifact`] in-process.
    ReleaseChecks,
}

fn cargo_step(args: &[&str], env: Vec<(String, String)>) -> GateAction {
    GateAction::Command {
        name: format!("cargo {}", args.join(" ")),
        program: "cargo".to_owned(),
        args: args.iter().map(ToString::to_string).collect(),
        env,
    }
}

/// Returns the ordered gate actions for one group name.
///
/// The mapping is pure data so tests can pin it without spawning anything.
#[must_use]
pub fn gate_actions(group: &str) -> Option<Vec<GateAction>> {
    let core = |all_targets: bool| {
        let mut actions = vec![
            cargo_step(&["fmt", "--all", "--", "--check"], Vec::new()),
            cargo_step(
                &["check", "--locked", "--workspace", "--all-features"],
                Vec::new(),
            ),
            cargo_step(
                &["test", "--locked", "--workspace", "--all-features"],
                Vec::new(),
            ),
            cargo_step(
                &[
                    "clippy",
                    "--locked",
                    "--workspace",
                    "--all-features",
                    "--",
                    "-D",
                    "warnings",
                ],
                Vec::new(),
            ),
            cargo_step(
                &["doc", "--locked", "--workspace", "--no-deps"],
                vec![("RUSTDOCFLAGS".to_owned(), "-D warnings".to_owned())],
            ),
        ];
        if all_targets {
            for action in actions.iter_mut().take(4).skip(1) {
                if let GateAction::Command { name, args, .. } = action {
                    let position = args
                        .iter()
                        .position(|argument| argument == "--all-features" || argument == "--")
                        .unwrap_or(args.len());
                    args.insert(position, "--all-targets".to_owned());
                    *name = format!("cargo {}", args.join(" "));
                }
            }
        }
        actions
    };
    match group {
        "core" => Some(core(true)),
        "core-fast" => Some(core(false)),
        "perf" => Some(vec![cargo_step(
            &[
                "bench",
                "--locked",
                "--workspace",
                "--all-features",
                "--benches",
            ],
            Vec::new(),
        )]),
        "vscode" => Some(vec![GateAction::Command {
            name: "npm run test:ci (editors/vscode)".to_owned(),
            program: "npm".to_owned(),
            args: vec![
                "--prefix".to_owned(),
                "editors/vscode".to_owned(),
                "run".to_owned(),
                "test:ci".to_owned(),
            ],
            env: Vec::new(),
        }]),
        "release" => Some(vec![
            cargo_step(
                &["build", "--locked", "-p", "pdc", "--bin", "pdc"],
                Vec::new(),
            ),
            GateAction::ReleaseChecks,
        ]),
        // Stable-only gates for the standalone fuzz crate; the nightly
        // cargo-fuzz run loop lives in CI so this group stays buildable
        // without a nightly toolchain.
        "fuzz" => Some(vec![
            cargo_step(
                &["fmt", "--manifest-path", "fuzz/Cargo.toml", "--", "--check"],
                Vec::new(),
            ),
            cargo_step(
                &[
                    "check",
                    "--locked",
                    "--manifest-path",
                    "fuzz/Cargo.toml",
                    "--all-targets",
                ],
                Vec::new(),
            ),
        ]),
        "all" => {
            let mut actions = Vec::new();
            for group in ["core", "vscode", "release", "fuzz"] {
                actions.extend(gate_actions(group)?);
            }
            Some(actions)
        }
        _ => None,
    }
}

/// Resolves the repository root for gate execution.
///
/// `--root` wins; otherwise the root is derived from `CARGO_MANIFEST_DIR`
/// (always set by `cargo run`, so the `cargo tools` alias just works).
pub fn resolve_root(root: Option<&Path>) -> Result<PathBuf, CliError> {
    if let Some(root) = root {
        if !root.join("Cargo.toml").is_file() {
            return Err(CliError::Usage(format!(
                "--root {} does not contain Cargo.toml",
                root.display()
            )));
        }
        return Ok(root.to_owned());
    }
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| {
            CliError::Usage(
                "gates requires --root when the binary runs outside cargo \
                 (or use: cargo tools -- gates)"
                    .to_owned(),
            )
        })?;
    let root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| CliError::Usage("cannot derive repository root".to_owned()))?
        .to_owned();
    if !root.join("Cargo.toml").is_file() {
        return Err(CliError::Usage(format!(
            "derived root {} does not contain Cargo.toml",
            root.display()
        )));
    }
    Ok(root)
}

/// Runs the named gate groups in order and returns the number of actions run.
///
/// Fails with [`CliError::CheckFailed`] at the first failing gate; spawned
/// commands inherit stdout/stderr so failures show exactly what CI shows.
pub fn run_gates(groups: &[String], root: &Path) -> Result<usize, CliError> {
    let mut executed = 0;
    for group in groups {
        let actions = gate_actions(group)
            .ok_or_else(|| CliError::Usage(format!("unknown gate group: {group}")))?;
        eprintln!("==> gates {group}");
        for action in actions {
            match action {
                GateAction::Command {
                    name,
                    program,
                    args,
                    env,
                } => {
                    eprintln!("==> {name}");
                    let mut command = spawn_base(&program);
                    command.args(&args).current_dir(root);
                    for (key, value) in env {
                        command.env(key, value);
                    }
                    let status = command
                        .status()
                        .map_err(|error| CliError::Exec(format!("{name}: {error}")))?;
                    executed += 1;
                    if !status.success() {
                        eprintln!("gates {group} failed at: {name}");
                        return Err(CliError::CheckFailed);
                    }
                }
                GateAction::ReleaseChecks => {
                    eprintln!("==> tools check release");
                    let results = crate::check::check_release_artifact(root);
                    executed += 1;
                    if !crate::check::report(&results) {
                        eprintln!("gates {group} failed at: tools check release");
                        return Err(CliError::CheckFailed);
                    }
                }
            }
        }
    }
    Ok(executed)
}

/// Builds the spawn command for a program, routing npm through `cmd /c` on
/// Windows where npm is a batch file rather than an executable.
fn spawn_base(program: &str) -> Command {
    if cfg!(windows) && program == "npm" {
        let mut command = Command::new("cmd");
        command.arg("/c").arg(program);
        command
    } else {
        Command::new(program)
    }
}

#[cfg(test)]
mod tests {
    use super::{GateAction, gate_actions};

    /// A `GateAction::Command` flattened for assertions: (name, args, env).
    type FlatCommand = (String, Vec<String>, Vec<(String, String)>);

    fn commands(group: &str) -> Vec<FlatCommand> {
        gate_actions(group)
            .expect("known group")
            .into_iter()
            .filter_map(|action| match action {
                GateAction::Command {
                    name, args, env, ..
                } => Some((name, args, env)),
                GateAction::ReleaseChecks => None,
            })
            .collect()
    }

    #[test]
    fn core_runs_the_full_gate_set() {
        let commands = commands("core");
        assert_eq!(commands.len(), 5, "{commands:?}");
        assert_eq!(
            commands[0].1,
            vec!["fmt", "--all", "--", "--check"],
            "formatting first for the fastest failure"
        );
        for (_, args, _) in commands.iter().take(4).skip(1) {
            assert!(
                args.contains(&"--all-targets".to_owned()),
                "core builds every target: {args:?}"
            );
        }
        assert!(
            commands
                .iter()
                .any(|(_, _, env)| env
                    .contains(&("RUSTDOCFLAGS".to_owned(), "-D warnings".to_owned()))),
            "docs gate treats warnings as errors"
        );
    }

    #[test]
    fn core_fast_skips_all_targets() {
        let commands = commands("core-fast");
        assert!(
            commands
                .iter()
                .all(|(_, args, _)| !args.contains(&"--all-targets".to_owned())),
            "core-fast must not build every target: {commands:?}"
        );
    }

    #[test]
    fn release_builds_pdc_then_reuses_the_release_checks() {
        let actions = gate_actions("release").expect("known group");
        assert!(matches!(actions[0], GateAction::Command { .. }));
        assert_eq!(actions[1], GateAction::ReleaseChecks);
        if let GateAction::Command { args, .. } = &actions[0] {
            assert!(
                args.windows(2).any(|pair| pair == ["-p", "pdc"]),
                "{args:?}"
            );
        }
    }

    #[test]
    fn all_expands_in_the_documented_order() {
        let mut expected = Vec::new();
        for group in ["core", "vscode", "release", "fuzz"] {
            expected.extend(gate_actions(group).expect("known group"));
        }
        assert_eq!(gate_actions("all").expect("known group"), expected);
    }

    #[test]
    fn unknown_groups_are_rejected() {
        assert!(gate_actions("typo").is_none());
    }
}
