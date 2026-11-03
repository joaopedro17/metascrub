/// What cleaning keeps beyond the strict default. `Default` is the safest choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Keep any color profile, not only standard ones (`--keep color-profile`).
    pub keep_color_profile: bool,
    /// Leave output files with the current time instead of 1980-01-01 (`--keep file-times`).
    pub keep_file_times: bool,
    /// Remove the Orientation tag even when the image is rotated (`--strip-orientation`).
    pub strip_orientation: bool,
    /// Keep Office comments, with authors replaced by a neutral name (`--keep comments`).
    pub keep_comments: bool,
    pub tracked_changes: TrackedChanges,
}

/// What to do with tracked changes in Word documents. Every choice changes
/// the text, so there is no safe default: files with tracked changes are
/// not cleaned until the user picks one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TrackedChanges {
    #[default]
    Undecided,
    /// Keep the text as it reads now (`--accept-changes`).
    Accept,
    /// Go back to the text before the changes (`--reject-changes`).
    Reject,
}
