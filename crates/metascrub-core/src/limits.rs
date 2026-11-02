/// Images are read fully into memory; anything larger is refused.
pub const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

/// A compressed metadata block (PNG `zTXt`, `iCCP`) may not inflate beyond
/// this, so a few bytes can't expand into gigabytes.
pub const MAX_INFLATED_BYTES: u64 = 16 * 1024 * 1024;

/// Files processed at the same time. Each may hold up to `MAX_IMAGE_BYTES`
/// in memory, so this bounds memory use as well as CPU.
pub const MAX_PARALLEL_FILES: usize = 4;

/// ZIP containers (Office files, later archives) are unpacked in memory, so
/// these bound what a small file can expand into (requirements §13.9).
pub const MAX_ZIP_ENTRIES: usize = 10_000;
pub const MAX_ZIP_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
/// Beyond this size, an entry may not unpack to more than 100 times its
/// compressed size: real documents stay well below, zip bombs don't.
pub const MAX_ZIP_RATIO: u64 = 100;
pub const ZIP_RATIO_CHECK_ABOVE: u64 = 1024 * 1024;
