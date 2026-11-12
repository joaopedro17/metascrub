# Changelog

What changed in each release of MetaScrub, newest first. From 1.0, versions follow [Semantic Versioning](https://semver.org); the [README](README.md#stability) says what that promises.

## Unreleased

### Added

- **MP3 files.** ID3v2 (2.2 to 2.4, at the start or with a footer at the end), ID3v1 and "TAG+", APE and Lyrics3 tags are removed: titles, artists, dates, comments, lyrics, cover art, loudness levels and every other tag. The sound is kept byte for byte, with the first frame that holds the length and seek table. An MP3 with other data among its frames, or with less than three frames of sound, is refused.
- **FLAC files.** Vorbis comments (every tag, and the encoder's name), pictures, application data, padding, and ID3 or APE tags added before or after are removed. The sound is kept byte for byte, with the stream description, seek table and cue sheet. Every frame's checksum is verified, so a file with other data among its frames is refused.
- **Ogg Vorbis and Opus files.** The comment header (every tag, cover art, loudness levels and the encoder's name) is replaced with an empty one, and ID3 or APE tags added after are removed. The sound is kept byte for byte; the pages that carry it get new page numbers and checksums. Opus's volume adjustment is kept, since players apply it. Every page's checksum is verified, so a file with other data among its pages, or with more than one stream, is refused.
- **WAV files.** Tags (`LIST INFO`), names and notes on cue points, Broadcast WAV metadata (originator, reference, dates, time of day, UMID, coding history), iXML, XMP, ID3 and every other chunk are removed, along with data after the end; the sampler's maker, model and SMPTE time are set to zero. The sound is kept byte for byte, with its format, cue points, sampler loops and instrument settings. WAV files over 4 GB (RF64) and object audio (ADM) are refused.
- **OpenDocument files** (`.odt`, `.ods`, `.odp`, `.odg` and templates). Document properties, the thumbnail, view and printer settings, editing-session IDs, comments (or only their authors and dates, with `--keep comments`) and stray files are removed; settings the layout depends on stay. Pictures and embedded charts are cleaned too. Macros and hidden content are reported. Files with tracked changes, signed and password-protected files are refused or skipped.
- **EPUB e-books.** Author, publisher, dates, other identifiers, descriptions, calibre and Apple Books extras and program names are removed; the title and language stay. The book's identifier is replaced by one computed from the cleaned book. Pictures are cleaned, and obfuscated fonts keep working. `--keep book-info` keeps the author, publisher and ISBN. Books with DRM are refused.
- **Diagnostic log:** `--log <file>` writes what MetaScrub did with each file (format, each step with its duration, memory, full errors) to attach to a problem report. It never contains metadata values and numbers files instead of naming them, unless `--show-values` is given. Nothing is logged without it; `METASCRUB_LOG=<level>` prints the messages to the terminal instead.

### Changed

- OpenDocument files and e-books inside ZIP and TAR archives are now cleaned; before, they were kept as they were.
- The README's speed table is now measured with the Linux download, and says how it differs from a build from source.

## 1.1.1 — 2026-11-07

### Fixed

- **Memory with several large PDFs, Office files or images, when MetaScrub is built from source on Linux** (`cargo install`). The system's memory allocator kept what each file freed, so four 400 MB PDFs used 4.4 GB instead of about 2 GB. MetaScrub now hands that memory back after each large file. The release downloads weren't affected.

## 1.1.0 — 2026-11-07

### Added

- **`--checksums`** shows the SHA-256 of each file read and each file written (the cleaned file or the copy), in the text report and as `sha256` in `--json`.
- **Cover art** in M4A files is reported as its own kind (`cover_art`, low risk) instead of "Another file embedded inside it". It's still removed.

### Changed

- **Every cleaned file is checked before it's kept.** MetaScrub reads it back as `verify` would; if anything it should have removed is left, nothing is written and the file fails with the new reason `incomplete` (in JSON). Cleaning takes up to about 20% longer; PDFs not at all, since their cleaning already read the result back.

## 1.0.0 — 2026-11-03

Everything since 0.3.0. From this release, the command-line options, exit codes and JSON report are stable.

### Added

- **PDF documents.** Removes the document information (author, title, dates, software), XMP, page thumbnails, application data, unused objects, data after the end and earlier versions left by incremental saves. The document ID is replaced by one computed from the cleaned file. Photos inside are cleaned like JPEG.
  - Comments are removed; `--keep comments` keeps them with every author shown as "Author".
  - Attachments are removed; `--keep attachments` keeps them, cleaned too when MetaScrub can read them.
  - Scripts and filled-in form fields are reported, not removed. Encrypted PDFs are skipped and digitally signed ones refused.
- **What the file system stores with a file.** Download addresses, Finder tags and comments kept in extended attributes (Linux, macOS) and in the `Zone.Identifier` stream (Windows) are reported for files and folders. Cleaned copies never carry them; `--in-place` removes them, even from files MetaScrub can't read.
- **Folders** get the neutral date too (1980-01-01) when they're cleaned in place or created in the `-o` folder, and appear in reports when something is found on them.
- **Junk files** that operating systems leave in folders (`.DS_Store`, `Thumbs.db`, `desktop.ini`, `._` files, `__MACOSX`) are reported. `--remove-junk` deletes them with `--in-place`, or leaves them out of the `-o` copy.
- **File names that leak** when or with what a file was made (`IMG_20261102_143512.jpg`, `PXL_…`, `WhatsApp Image …`, `Screenshot …`) are reported. `--rename` replaces them with neutral names such as `image-001.jpg`, `video-001.mp4` or `audio-001.m4a`.
- **ZIP, TAR and gzip archives** (`.zip`, `.tar`, `.tar.gz`, `.tgz`, `.gz`). Every file inside is cleaned with the same rules, archives inside archives too (3 levels deep). Dates, owners, permissions, comments and extra fields are reset; junk files inside are removed; files MetaScrub can't read are kept as they are and reported. Archives of any size are processed a file at a time. Zip bombs, archives over 4 GB unpacked, and archives with paths leading outside them (`../`) are refused; encrypted ones are skipped.
- **MP4, QuickTime (`.mov`) and M4A files.** Location, device make and model, software and encoder names, titles, names, comments and cover art are removed, and header dates set to zero, without moving a byte, so the file still plays. GPS and sensor tracks from GoPro, DJI, Google's camera motion and Apple's timed metadata, and timecode tracks, are zeroed. Subtitles and chapters aren't checked; they're reported when present.
- **HEIC and AVIF photos.** EXIF is rewritten with only the orientation and XMP emptied, in place. Previews, depth maps and HDR gain maps are kept and reported as low risk.
- **Tab completion and manual pages.** `metascrub completions <shell>` prints a completion script for Bash, Zsh, Fish or PowerShell, and `metascrub man [command]` a manual page. Both are included in the release downloads.
- The full `--help` lists the exit codes.
- **Verifiable downloads.** Every archive has a build attestation, so `gh attestation verify` can show it was built by this project's GitHub workflow from its source, and a software bill of materials (`.cdx.json`, CycloneDX) listing every library in it.

### Changed

- Files are streamed through cleaning instead of being held in memory, so large files use little memory.
- Large images, PDFs and Office files wait for each other instead of being processed together, so memory stays around 2 GB at most (four 400 MB PDFs used 6.9 GB before).
- Office files with parts whose paths lead outside them now fail as `unsafe_path` (in JSON) instead of `malformed`, with a message that says why.
- Junk files and leaking file names say which option fixes them, instead of "can't be fixed automatically".
- "Data after the end of the image" is now "data added after the file's content", since it also applies to videos and documents.
- A Word document that needs a decision about tracked changes says what will be removed once you decide, and the summary counts it as needing your decision instead of failed. In JSON it's still counted in `summary.failed`.
- Dates that are removed and dates that are reset in the same file are listed once.
- Reports that list nothing no longer end with hints about hidden values or `--rename`.

### JSON report

Added, with `schema_version` still 1:

- Formats: `pdf`, `tar`, `gzip`, `mp4`, `mov`, `m4a`, `heic`, `avif` and `folder` (a folder reported for what the file system stores with it).
- Finding kinds: `earlier_versions`, `form_data`, `unread_file`, `symbolic_link`, `text_track`, `junk_file`, `file_attribute` and `file_name`.
- Reasons for keeping something: `not_written` and `part_of_image`.
- Outcomes: `removed` and `would_remove` (junk files), counted in the new `summary.removed`.
- Failure kind: `unsafe_path`.

## 0.3.0 — 2026-11-03

### Added

- **Word, Excel and PowerPoint documents** (`.docx`, `.xlsx`, `.pptx` and their macro-enabled and template variants). Removes document properties (author, company, dates, custom properties), the thumbnail, revision and document IDs, template and folder paths, printer settings, people lists, sensitivity labels, unknown parts and ZIP dates. Pictures inside are cleaned like the image formats.
- **Comments** in Office documents are removed; `--keep comments` keeps them with every author shown as "Author".
- **Tracked changes** in Word block cleaning until you choose `--accept-changes` or `--reject-changes`; either way, the history and the reviewers' names are removed.
- Embedded Office documents (such as the workbook behind a chart) are cleaned too. Macros, other embedded files and hidden content are reported.
- Links to other workbooks keep only the file name.
- Old-format and password-protected Office files are recognized and skipped with a reason.

## 0.2.0 — 2026-11-02

The first release.

### Added

- `inspect` lists hidden metadata, `clean` removes it, and `verify` checks that nothing removable is left.
- **JPEG, PNG and WebP** images, including animated PNG and WebP: EXIF (GPS, camera, serial numbers, dates, names), thumbnails, XMP, IPTC and Photoshop data, comments, text chunks, hidden secondary images, Motion Photo videos and other data after the image.
- Rotated photos keep only their rotation (`--strip-orientation` removes it), and standard color profiles are kept (`--keep color-profile` keeps any).
- Whole folders with `-r`; `-o` copies the folder structure into another folder, including files MetaScrub can't read, which are flagged.
- Cleaned copies are written next to the originals (`photo.clean.jpg`) unless `-o` or `--in-place` is given; existing files are never overwritten, and files are written completely or not at all, dated 1980-01-01 (`--keep file-times` keeps the current date).
- Reports hide values unless `--show-values` is given; `--json` for scripts; exit codes for scripts (`verify` exits with 4 when metadata remains).
- Several files are cleaned at once, with a progress bar; Ctrl+C stops cleanly.
- Downloads for Windows, macOS and Linux (Intel and ARM), with checksums and the licenses of the libraries MetaScrub uses.
