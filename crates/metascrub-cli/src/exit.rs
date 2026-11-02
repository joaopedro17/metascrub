//! Exit codes (requirements §13.6). `2` (invalid usage) comes from clap.

use std::process::ExitCode;

use metascrub_core::model::RunReport;

const SOME_FAILED: u8 = 1;
const ALL_FAILED: u8 = 3;
const METADATA_REMAINS: u8 = 4;

pub fn exit_code(report: &RunReport) -> ExitCode {
    let summary = report.summary();
    if summary.failed > 0 && summary.failed == report.files.len() {
        ExitCode::from(ALL_FAILED)
    } else if summary.failed > 0 {
        ExitCode::from(SOME_FAILED)
    } else if summary.remaining_metadata > 0 {
        ExitCode::from(METADATA_REMAINS)
    } else {
        ExitCode::SUCCESS
    }
}
