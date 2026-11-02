/// What cleaning keeps beyond the strict default. `Default` is the safest choice.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Options {
    /// Keep any color profile, not only standard ones (`--keep color-profile`).
    pub keep_color_profile: bool,
    /// Leave output files with the current time instead of 1980-01-01 (`--keep file-times`).
    pub keep_file_times: bool,
    /// Remove the Orientation tag even when the image is rotated (`--strip-orientation`).
    pub strip_orientation: bool,
}
