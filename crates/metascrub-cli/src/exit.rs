//! Exit codes (requirements §13.6). `2` (invalid usage) comes from clap.

use std::process::ExitCode;

use metascrub_core::model::RunReport;

const SOME_FAILED: u8 = 1;
const ALL_FAILED: u8 = 3;

pub fn exit_code(report: &RunReport) -> ExitCode {
    let failed = report.summary().failed;
    if failed == 0 {
        ExitCode::SUCCESS
    } else if failed == report.files.len() {
        ExitCode::from(ALL_FAILED)
    } else {
        ExitCode::from(SOME_FAILED)
    }
}
