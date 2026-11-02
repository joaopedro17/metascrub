pub mod human;
pub mod json;
pub mod messages;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Inspect,
    Clean { dry_run: bool },
    Verify,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Clean { .. } => "clean",
            Self::Verify => "verify",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub mode: Mode,
    pub show_values: bool,
    pub verbosity: Verbosity,
}
