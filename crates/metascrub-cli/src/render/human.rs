use std::io::{self, Write};

use anstyle::{AnsiColor, Style};
use metascrub_core::model::{
    Disposition, FailureKind, FileReport, Finding, FindingKind, KeepReason, Location, Outcome,
    Risk, RunReport, Summary,
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
        // Listed together in the summary instead.
        if matches!(&file.outcome, Outcome::Failed(f) if f.kind == FailureKind::Cancelled) {
            continue;
        }
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
        render_checksums(file, out)?;
        writeln!(out)?;
    }
    render_summary(report, settings, out)
}

fn render_file(file: &FileReport, settings: &Settings, out: &mut dyn Write) -> io::Result<()> {
    let path = messages::path(&file.path);
    let arrow = match &file.output {
        Some(output) if output != &file.path => format!(" → {}", messages::path(output)),
        _ => String::new(),
    };
    let kind = file.format.map(messages::format).unwrap_or("file");
    match &file.outcome {
        Outcome::Skipped(reason) => {
            let reason = messages::skip_reason(*reason, file.format);
            // A file MetaScrub can't read still moves to its new name.
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#}{arrow} {WARNING}skipped: {reason}{WARNING:#}"
            )?;
            // What the file system stores with a file MetaScrub can't read.
            render_findings(&file.findings, settings, out)?;
            let not_written = Disposition::Keep(KeepReason::NotWritten);
            if file.findings.iter().any(|f| f.disposition == not_written) {
                writeln!(out, "  {}", messages::ATTRIBUTES_HINT)?;
            }
            Ok(())
        }
        Outcome::Failed(failure) => {
            let message = messages::failure(failure.kind);
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#} {ERROR}failed: {message}{ERROR:#}"
            )?;
            if settings.verbosity == Verbosity::Verbose {
                writeln!(out, "  {DIM}{}{DIM:#}", messages::sanitize(&failure.detail))?;
            }
            Ok(())
        }
        Outcome::NeedsDecision(decision) => {
            writeln!(
                out,
                "{BOLD}{path}{BOLD:#} {DIM}({kind}){DIM:#} {ERROR}needs your decision{ERROR:#}"
            )?;
            // Nothing was written, so say what cleaning will do once decided.
            let tense = match settings.mode {
                Mode::Clean { dry_run: true } => Tense::Conditional,
                _ => Tense::Future,
            };
            render_findings_in(&file.findings, tense, settings, out)?;
            writeln!(out, "  {}", messages::decision(*decision))
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
        Outcome::Removed => writeln!(out, "{BOLD}{path}{BOLD:#} {}", messages::REMOVED),
        Outcome::WouldRemove => writeln!(out, "{BOLD}{path}{BOLD:#} {}", messages::WOULD_REMOVE),
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

fn render_checksums(file: &FileReport, out: &mut dyn Write) -> io::Result<()> {
    let Some(checksums) = &file.checksums else {
        return Ok(());
    };
    match &checksums.output {
        Some(output) => {
            writeln!(
                out,
                "  {DIM}{}: {}{DIM:#}",
                messages::CHECKSUM_BEFORE,
                checksums.original
            )?;
            // Lined up under the value above.
            writeln!(out, "  {DIM}{}:  {output}{DIM:#}", messages::CHECKSUM_AFTER)
        }
        None => writeln!(
            out,
            "  {DIM}{}: {}{DIM:#}",
            messages::CHECKSUM,
            checksums.original
        ),
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
    render_findings_in(findings, tense, settings, out)
}

fn render_findings_in(
    findings: &[Finding],
    tense: Tense,
    settings: &Settings,
    out: &mut dyn Write,
) -> io::Result<()> {
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
            let disposition = if line.also_reset {
                messages::removed_or_reset(tense)
            } else {
                messages::disposition(line.kind, line.disposition, tense)
            };
            write!(out, " {DIM}— {disposition}{DIM:#}")?;
            if let Some(location) = line.location {
                write!(
                    out,
                    " {DIM}[{}]{DIM:#}",
                    messages::sanitize(&location.to_string())
                )?;
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
    /// Some of the merged findings are reset rather than removed.
    also_reset: bool,
    values: Vec<String>,
    location: Option<&'a Location>,
}

/// One line per finding in verbose mode. Otherwise findings that read the same
/// ("Date and time — will be removed") are merged and their values joined, as
/// are removed and reset findings of one kind, which would otherwise read like
/// the same thing listed twice.
fn lines<'a>(findings: &[&'a Finding], verbose: bool) -> Vec<Line<'a>> {
    let mut lines: Vec<Line<'a>> = Vec::new();
    for finding in findings {
        let value = messages::value(finding);
        if !verbose
            && let Some(line) = lines
                .iter_mut()
                .find(|l| l.kind == finding.kind && merges(l.disposition, finding.disposition))
        {
            if line.disposition != finding.disposition {
                line.disposition = Disposition::Remove;
                line.also_reset = true;
            }
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
            also_reset: false,
            values: value.into_iter().collect(),
            location: verbose.then_some(&finding.location),
        });
    }
    lines
}

fn merges(a: Disposition, b: Disposition) -> bool {
    let changed = |d| matches!(d, Disposition::Remove | Disposition::Normalize);
    a == b || (changed(a) && changed(b))
}

fn render_summary(report: &RunReport, settings: &Settings, out: &mut dyn Write) -> io::Result<()> {
    let summary = report.summary();
    let mut parts = match settings.mode {
        Mode::Inspect => {
            let with_metadata = report
                .files
                .iter()
                .filter(|f| {
                    f.outcome == Outcome::Inspected
                        && f.findings
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
            let (cleaned, copied, removed) = if dry_run {
                (
                    "would be cleaned",
                    "would be copied without cleaning",
                    "would be removed",
                )
            } else {
                ("cleaned", "copied without cleaning", "removed")
            };
            let mut parts = vec![format!("{} {cleaned}", files(summary.cleaned))];
            if summary.copied > 0 {
                parts.push(format!("{} {copied}", summary.copied));
            }
            if summary.removed > 0 {
                let junk = if summary.removed == 1 {
                    "junk file"
                } else {
                    "junk files"
                };
                parts.push(format!("{} {junk} {removed}", summary.removed));
            }
            parts
        }
        Mode::Verify => vec![format!(
            "{} verified clean, {} still with hidden metadata",
            files(summary.verified),
            summary.remaining_metadata
        )],
    };
    parts.extend(problems(report, &summary));
    writeln!(out, "{BOLD}{}{BOLD:#}", parts.join(", "))?;

    if settings.verbosity == Verbosity::Quiet {
        return Ok(());
    }
    let shown: Vec<&Finding> = report
        .files
        .iter()
        .flat_map(|f| shown(f, settings))
        .collect();
    let has_hidden_values = !settings.show_values && shown.iter().any(|f| f.value.is_some());
    if has_hidden_values {
        writeln!(
            out,
            "{DIM}Values are hidden so this report is safe to share. Use --show-values to see them.{DIM:#}"
        )?;
    }
    let left = |kind| {
        shown
            .iter()
            .any(|f| f.kind == kind && f.disposition == Disposition::Warn)
    };
    if left(FindingKind::JunkFile) {
        writeln!(out, "{DIM}{}{DIM:#}", messages::JUNK_HINT)?;
    }
    if left(FindingKind::FileName) {
        writeln!(out, "{DIM}{}{DIM:#}", messages::NAME_HINT)?;
    }
    if settings.mode == (Mode::Clean { dry_run: false }) && summary.cleaned > 0 {
        writeln!(out, "{DIM}{}{DIM:#}", messages::LIMITATIONS)?;
    }
    Ok(())
}

/// The findings `render_file` lists for a file.
fn shown<'a>(file: &'a FileReport, settings: &Settings) -> Vec<&'a Finding> {
    match file.outcome {
        Outcome::Verified if settings.verbosity != Verbosity::Verbose => Vec::new(),
        Outcome::RemainingMetadata => file
            .findings
            .iter()
            .filter(|f| !f.disposition.is_acceptable_after_cleaning())
            .collect(),
        Outcome::Failed(_)
        | Outcome::Copied
        | Outcome::WouldCopy
        | Outcome::Removed
        | Outcome::WouldRemove => Vec::new(),
        _ => file.findings.iter().collect(),
    }
}

fn problems(report: &RunReport, summary: &Summary) -> Vec<String> {
    let mut parts = Vec::new();
    if summary.skipped > 0 {
        parts.push(format!("{} skipped", summary.skipped));
    }
    // Counted as failed in JSON, but nothing went wrong.
    let decisions = report
        .files
        .iter()
        .filter(|f| matches!(f.outcome, Outcome::NeedsDecision(_)))
        .count();
    if decisions > 0 {
        let verb = if decisions == 1 { "needs" } else { "need" };
        parts.push(format!("{decisions} {verb} your decision"));
    }
    let failed = summary.failed - decisions;
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if summary.cancelled > 0 {
        parts.push(format!(
            "{} not processed because the run was stopped",
            summary.cancelled
        ));
    }
    parts
}

fn files(n: usize) -> String {
    format!("{n} file{}", if n == 1 { "" } else { "s" })
}
