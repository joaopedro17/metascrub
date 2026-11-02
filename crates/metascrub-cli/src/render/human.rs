use std::io::{self, Write};

use anstyle::{AnsiColor, Style};
use metascrub_core::model::{
    Disposition, FileReport, Finding, FindingKind, Location, Outcome, Risk, RunReport, Summary,
};

use super::messages::{self, Tense};
use super::{Mode, Settings, Verbosity};

const BOLD: Style = Style::new().bold();
const DIM: Style = Style::new().dimmed();
const HIGH: Style = AnsiColor::Red.on_default().bold();
const MEDIUM: Style = AnsiColor::Yellow.on_default().bold();
const LOW: Style = Style::new().bold();
const GOOD: Style = AnsiColor::Green.on_default();
const WARNING: Style = AnsiColor::Yellow.on_default();
const ERROR: Style = AnsiColor::Red.on_default();

pub fn render(report: &RunReport, settings: &Settings, out: &mut dyn Write) -> io::Result<()> {
    for file in &report.files {
        let needs_attention = matches!(
            file.outcome,
            Outcome::Failed(_)
                | Outcome::Skipped(_)
                | Outcome::NeedsDecision(_)
                | Outcome::RemainingMetadata
                | Outcome::Copied
                | Outcome::WouldCopy
        );
        if settings.verbosity == Verbosity::Quiet && !needs_attention {
            continue;
        }
        render_file(file, settings, out)?;
        writeln!(out)?;
    }
    render_summary(report, settings, out)
}

fn render_file(file: &FileReport, settings: &Settings, out: &mut dyn Write) -> io::Result<()> {
    let path = file.path.display();
    let arrow = match &file.output {
        Some(output) if output != &file.path => format!(" → {}", output.display()),
        _ => String::new(),
    };
    let kind = file.format.map(messages::format).unwrap_or("file");
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
        Outcome::NeedsDecision(_) => {
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#} {ERROR}needs your decision{ERROR:#}"
            )?;
            render_findings(&file.findings, settings, out)
        }
        Outcome::Copied => writeln!(
            out,
            "{BOLD}{path}{BOLD:#}{arrow} {WARNING}{}{WARNING:#}",
            messages::COPIED
        ),
        Outcome::WouldCopy => writeln!(
            out,
            "{BOLD}{path}{BOLD:#} {WARNING}{}{WARNING:#}",
            messages::WOULD_COPY
        ),
        Outcome::Verified => {
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#} {DIM}({kind}){DIM:#} {GOOD}{}{GOOD:#}",
                messages::VERIFIED
            )?;
            if settings.verbosity == Verbosity::Verbose {
                render_findings(&file.findings, settings, out)?;
            }
            Ok(())
        }
        Outcome::RemainingMetadata => {
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#} {DIM}({kind}){DIM:#} {ERROR}{}{ERROR:#}",
                messages::REMAINING
            )?;
            let remaining: Vec<_> = file
                .findings
                .iter()
                .filter(|f| !f.disposition.is_acceptable_after_cleaning())
                .cloned()
                .collect();
            render_findings(&remaining, settings, out)
        }
        Outcome::Inspected | Outcome::Cleaned | Outcome::WouldClean => {
            writeln!(out, "{BOLD}{path}{BOLD:#}{arrow} {DIM}({kind}){DIM:#}")?;
            if file.findings.is_empty() {
                return writeln!(out, "  No hidden metadata found.");
            }
            render_findings(&file.findings, settings, out)
        }
    }
}

fn render_findings(
    findings: &[Finding],
    settings: &Settings,
    out: &mut dyn Write,
) -> io::Result<()> {
    let tense = match settings.mode {
        Mode::Inspect | Mode::Verify => Tense::Future,
        Mode::Clean { dry_run: true } => Tense::Conditional,
        Mode::Clean { dry_run: false } => Tense::Past,
    };
    let verbose = settings.verbosity == Verbosity::Verbose;
    for (risk, label, style) in [
        (Risk::High, "High risk", HIGH),
        (Risk::Medium, "Medium risk", MEDIUM),
        (Risk::Low, "Low risk", LOW),
    ] {
        let findings: Vec<_> = findings.iter().filter(|f| f.risk == risk).collect();
        if findings.is_empty() {
            continue;
        }
        writeln!(out, "  {style}{label}{style:#}")?;
        for line in lines(&findings, verbose) {
            write!(out, "    • {}", messages::finding(line.kind))?;
            if settings.show_values && !line.values.is_empty() {
                write!(out, ": {}", line.values.join("; "))?;
            }
            write!(
                out,
                " {DIM}— {}{DIM:#}",
                messages::disposition(line.disposition, tense)
            )?;
            if let Some(location) = line.location {
                write!(out, " {DIM}[{location}]{DIM:#}")?;
            }
            writeln!(out)?;
        }
    }
    Ok(())
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
    let mut parts = match settings.mode {
        Mode::Inspect => {
            let with_metadata = report
                .files
                .iter()
                .filter(|f| {
                    f.findings
                        .iter()
                        .any(|finding| !finding.disposition.is_acceptable_after_cleaning())
                })
                .count();
            vec![format!(
                "{} inspected, {with_metadata} with hidden metadata",
                files(summary.inspected)
            )]
        }
        Mode::Clean { dry_run } => {
            let (cleaned, copied) = if dry_run {
                ("would be cleaned", "would be copied without cleaning")
            } else {
                ("cleaned", "copied without cleaning")
            };
            let mut parts = vec![format!("{} {cleaned}", files(summary.cleaned))];
            if summary.copied > 0 {
                parts.push(format!("{} {copied}", summary.copied));
            }
            parts
        }
        Mode::Verify => vec![format!(
            "{} verified clean, {} still with hidden metadata",
            files(summary.verified),
            summary.remaining_metadata
        )],
    };
    parts.extend(problems(&summary));
    writeln!(out, "{BOLD}{}{BOLD:#}", parts.join(", "))?;

    if settings.verbosity == Verbosity::Quiet {
        return Ok(());
    }
    let has_hidden_values = !settings.show_values
        && report
            .files
            .iter()
            .flat_map(|f| &f.findings)
            .any(|f| f.value.is_some());
    if has_hidden_values {
        writeln!(
            out,
            "{DIM}Values are hidden so this report is safe to share. Use --show-values to see them.{DIM:#}"
        )?;
    }
    if settings.mode == (Mode::Clean { dry_run: false }) && summary.cleaned > 0 {
        writeln!(out, "{DIM}{}{DIM:#}", messages::LIMITATIONS)?;
    }
    Ok(())
}

fn problems(summary: &Summary) -> Vec<String> {
    let mut parts = Vec::new();
    if summary.skipped > 0 {
        parts.push(format!("{} skipped", summary.skipped));
    }
    if summary.failed > 0 {
        parts.push(format!("{} failed", summary.failed));
    }
    parts
}

fn files(n: usize) -> String {
    format!("{n} file{}", if n == 1 { "" } else { "s" })
}
