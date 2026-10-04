//! CI and release control without linking the analyzer or embedding game rules.
use std::fmt;

pub fn execute(args: &[String]) -> Result<String, CliError> {
    match args {
        [group, rest @ ..] if group == "ci" || group == "fuzz" => {
            crate::ci::execute(group, rest).map_err(CliError::Exec)
        }
        [group, command, rest @ ..] if group == "release" => {
            crate::cli_release::execute(command, rest)
        }
        [help] if help == "--help" || help == "-h" => Ok(
            "tools (without analysis): ci ... | release package|verify ... | fuzz smoke ...".into(),
        ),
        _ => Err(CliError::Usage(
            "this command requires the default analysis feature; omit --no-default-features".into(),
        )),
    }
}

#[derive(Debug)]
pub enum CliError {
    Usage(String),
    Exec(String),
}
impl CliError {
    pub const fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Exec(_) => 1,
        }
    }
}
impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(s) | Self::Exec(s) => f.write_str(s),
        }
    }
}
impl std::error::Error for CliError {}
