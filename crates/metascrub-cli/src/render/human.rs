use std::io::{self, Write};

use anstyle::{AnsiColor, Style};
use metascrub_core::model::{
    Disposition, FileReport, Finding, FindingKind, Location, Outcome, Risk, RunReport,
};

use super::{Settings, Verbosity, messages};

const BOLD: Style = Style::new().bold();
const DIM: Style = Style::new().dimmed();
const HIGH: Style = AnsiColor::Red.on_default().bold();
const MEDIUM: Style = AnsiColor::Yellow.on_default().bold();
const LOW: Style = Style::new().bold();
const WARNING: Style = AnsiColor::Yellow.on_default();
const ERROR: Style = AnsiColor::Red.on_default();

pub fn render(report: &RunReport, settings: &Settings, out: &mut dyn Write) -> io::Result<()> {
    for file in &report.files {
        let is_problem = matches!(file.outcome, Outcome::Failed(_) | Outcome::Skipped(_));
        if settings.verbosity == Verbosity::Quiet && !is_problem {
            continue;
        }
        render_file(file, settings, out)?;
        writeln!(out)?;
    }
    render_summary(report, settings, out)
}

fn render_file(file: &FileReport, settings: &Settings, out: &mut dyn Write) -> io::Result<()> {
    let path = file.path.display();
    match &file.outcome {
        Outcome::Skipped(reason) => {
            let reason = messages::skip_reason(*reason, file.format);
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#} {WARNING}skipped: {reason}{WARNING:#}"
            )
        }
        Outcome::Failed(failure) => {
            let message = messages::failure(failure.kind);
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#} {ERROR}failed: {message}{ERROR:#}"
            )?;
            if settings.verbosity == Verbosity::Verbose {
                writeln!(out, "  {DIM}{}{DIM:#}", failure.detail)?;
            }
            Ok(())
        }
        _ => {
            let kind = file.format.map(messages::format).unwrap_or("file");
            writeln!(out, "{BOLD}{path}{BOLD:#} {DIM}({kind}){DIM:#}")?;
            if file.findings.is_empty() {
                return writeln!(out, "  No hidden metadata found.");
            }
            for (risk, label, style) in [
                (Risk::High, "High risk", HIGH),
                (Risk::Medium, "Medium risk", MEDIUM),
                (Risk::Low, "Low risk", LOW),
            ] {
                let findings: Vec<_> = file.findings.iter().filter(|f| f.risk == risk).collect();
                if findings.is_empty() {
                    continue;
                }
                writeln!(out, "  {style}{label}{style:#}")?;
                let verbose = settings.verbosity == Verbosity::Verbose;
                for line in lines(&findings, verbose) {
                    write!(out, "    • {}", messages::finding(line.kind))?;
                    if settings.show_values && !line.values.is_empty() {
                        write!(out, ": {}", line.values.join("; "))?;
                    }
                    write!(
                        out,
                        " {DIM}— {}{DIM:#}",
                        messages::disposition(line.disposition)
                    )?;
                    if let Some(location) = line.location {
                        write!(out, " {DIM}[{location}]{DIM:#}")?;
                    }
                    writeln!(out)?;
                }
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Line<'a> {
    kind: FindingKind,
    disposition: Disposition,
    values: Vec<String>,
    location: Option<&'a Location>,
}

/// One line per finding in verbose mode. Otherwise findings that read the same
/// ("Date and time — will be removed") are merged and their values joined.
fn lines<'a>(findings: &[&'a Finding], verbose: bool) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    for finding in findings {
        let value = messages::value(finding);
        if !verbose
            && let Some(line) = lines
                .iter_mut()
                .find(|l| l.kind == finding.kind && l.disposition == finding.disposition)
        {
            if let Some(value) = value
                && !line.values.contains(&value)
            {
                line.values.push(value);
            }
            continue;
        }
        lines.push(Line {
            kind: finding.kind,
            disposition: finding.disposition,
            values: value.into_iter().collect(),
            location: verbose.then_some(&finding.location),
        });
    }
    lines
}

fn render_summary(report: &RunReport, settings: &Settings, out: &mut dyn Write) -> io::Result<()> {
    let summary = report.summary();
    let with_metadata = report
        .files
        .iter()
        .filter(|f| {
            f.findings
                .iter()
                .any(|finding| !finding.disposition.is_acceptable_after_cleaning())
        })
        .count();

    let files = |n: usize| format!("{n} file{}", if n == 1 { "" } else { "s" });
    let mut parts = vec![format!(
        "{} inspected, {with_metadata} with hidden metadata",
        files(summary.inspected)
    )];
    if summary.skipped > 0 {
        parts.push(format!("{} skipped", summary.skipped));
    }
    if summary.failed > 0 {
        parts.push(format!("{} failed", summary.failed));
    }
    writeln!(out, "{BOLD}{}{BOLD:#}", parts.join(", "))?;

    let has_hidden_values = !settings.show_values
        && report
            .files
            .iter()
            .flat_map(|f| &f.findings)
            .any(|f| f.value.is_some());
    if has_hidden_values && settings.verbosity != Verbosity::Quiet {
        writeln!(
            out,
            "{DIM}Values are hidden so this report is safe to share. Use --show-values to see them.{DIM:#}"
        )?;
    }
    Ok(())
}
