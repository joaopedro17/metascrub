//! Shell completion scripts and manual pages, generated from the same
//! definitions as `--help` so they can't drift apart.

use std::io::{self, Write};

use clap::{CommandFactory, ValueEnum};
use clap_complete::Shell as Generator;

use crate::args::{Cli, Page, Shell};

pub fn completions(shell: Shell, out: &mut dyn Write) -> io::Result<()> {
    let generator = match shell {
        Shell::Bash => Generator::Bash,
        Shell::Zsh => Generator::Zsh,
        Shell::Fish => Generator::Fish,
        Shell::Powershell => Generator::PowerShell,
    };
    let mut command = Cli::command();
    let mut buffer = Vec::new();
    clap_complete::generate(generator, &mut command, "metascrub", &mut buffer);
    out.write_all(&buffer)
}

pub fn manual(page: Option<Page>, out: &mut dyn Write) -> io::Result<()> {
    let mut command = Cli::command();
    // Building names each command `metascrub-clean`, as its page is called.
    command.build();
    let command = match page {
        None => command,
        Some(page) => {
            let name = page
                .to_possible_value()
                .map(|value| value.get_name().to_owned())
                .unwrap_or_default();
            command
                .find_subcommand(&name)
                .cloned()
                .ok_or_else(|| io::Error::other(format!("no command {name}")))?
        }
    };
    clap_mangen::Man::new(command)
        .source(format!("metascrub {}", env!("CARGO_PKG_VERSION")))
        .render(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(write: impl Fn(&mut Vec<u8>) -> io::Result<()>) -> String {
        let mut out = Vec::new();
        write(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn completions_know_every_command_and_option() {
        for shell in Shell::value_variants() {
            let script = text(|out| completions(*shell, out));
            for word in [
                "inspect",
                "clean",
                "verify",
                "remove-junk",
                "strip-orientation",
            ] {
                assert!(script.contains(word), "{shell:?} lacks {word}");
            }
        }
    }

    #[test]
    fn every_command_has_a_manual_page() {
        let main = text(|out| manual(None, out));
        assert!(main.contains(".TH metascrub 1"), "{main}");
        assert!(main.contains("metascrub\\-clean(1)"), "{main}");
        for page in Page::value_variants() {
            let name = page.to_possible_value().unwrap().get_name().to_owned();
            let text = text(|out| manual(Some(*page), out));
            let title = format!(
                ".TH metascrub-{name} 1  \"metascrub {}\"",
                env!("CARGO_PKG_VERSION")
            );
            assert!(text.contains(&title), "{text}");
        }
        let clean = text(|out| manual(Some(Page::Clean), out));
        assert!(clean.contains("\\-\\-in\\-place"), "{clean}");
    }
}
