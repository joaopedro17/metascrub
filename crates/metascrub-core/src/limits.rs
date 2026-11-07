/// Images are read fully into memory; anything larger is refused.
pub const MAX_IMAGE_BYTES: u64 = 512 * 1024 * 1024;

/// A compressed metadata block (PNG `zTXt`, `iCCP`) may not inflate beyond
/// this, so a few bytes can't expand into gigabytes.
pub const MAX_INFLATED_BYTES: u64 = 16 * 1024 * 1024;

/// Files processed at the same time.
pub const MAX_PARALLEL_FILES: usize = 4;
/// Files read whole into memory (images, PDF, Office) may together take
/// about this much while they're processed; larger ones wait their turn.
pub const MEMORY_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// After a file estimated to take at least this much memory, the memory
/// it freed is handed back to the system.
pub const LARGE_FILE_MEMORY_BYTES: u64 = 256 * 1024 * 1024;

/// ZIP containers and archives hold at most this many files, so a small file
/// can't expand into millions (requirements §13.9).
pub const MAX_ZIP_ENTRIES: usize = 10_000;
/// Office files are unpacked in memory, so they're bounded more tightly than
/// archives.
pub const MAX_ZIP_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
/// Beyond this size, an entry (or a whole `.gz` file) may not unpack to more
/// than 100 times its compressed size: real files stay well below, zip bombs
/// don't.
pub const MAX_ZIP_RATIO: u64 = 100;
pub const ZIP_RATIO_CHECK_ABOVE: u64 = 1024 * 1024;

/// What an archive, with every archive inside it, may unpack to in total.
pub const MAX_ARCHIVE_UNPACKED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Files inside archives are held in memory up to this size, then in a
/// temporary file.
pub const SPOOL_IN_MEMORY_BYTES: usize = 16 * 1024 * 1024;
/// TAR's extra headers (long names, PAX records) are read into memory.
pub const MAX_TAR_HEADER_BYTES: u64 = 1024 * 1024;

/// Metadata boxes in videos and photos (and the sample tables of the tracks
/// cleaning blanks out) are read into memory up to this size. Pictures and
/// sound never are.
pub const MAX_MEDIA_BOX_BYTES: u64 = 64 * 1024 * 1024;
/// A box may hold at most this many boxes.
pub const MAX_MEDIA_BOXES: usize = 100_000;

/// A PDF's object and cross-reference streams are unpacked while it's
/// read, and attachments when they're cleaned; none may unpack beyond this.
pub const MAX_PDF_STREAM_BYTES: usize = 64 * 1024 * 1024;

/// Documents embedded in documents (a chart's workbook inside a Word file)
/// are cleaned too, but only this many levels deep.
pub const MAX_EMBEDDING_DEPTH: usize = 3;
