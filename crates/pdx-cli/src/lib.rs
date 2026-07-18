//! User-facing command-line entry point helpers.

/// Runs the `pdx` command and returns its current user-facing message.
#[must_use]
pub fn run_pdx(args: &[String]) -> String {
    if args.iter().any(|argument| argument == "--version" || argument == "-V") {
        "pdx 0.1.0".to_owned()
    } else {
        "pdx 0.1.0\nWorkspace analysis commands are scheduled for later phases.".to_owned()
    }
}
