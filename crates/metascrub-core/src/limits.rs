/// Images are read fully into memory; anything larger is refused.
pub const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

/// A compressed metadata block (PNG `zTXt`, `iCCP`) may not inflate beyond
/// this, so a few bytes can't expand into gigabytes.
pub const MAX_INFLATED_BYTES: u64 = 16 * 1024 * 1024;

/// Files processed at the same time. Each may hold up to `MAX_IMAGE_BYTES`
/// in memory, so this bounds memory use as well as CPU.
pub const MAX_PARALLEL_FILES: usize = 4;
