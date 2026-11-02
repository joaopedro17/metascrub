pub mod human;
pub mod json;
mod messages;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verbosity {
    Quiet,
    Normal,
    Verbose,
}

#[derive(Debug, Clone, Copy)]
pub struct Settings {
    pub show_values: bool,
    pub verbosity: Verbosity,
}
