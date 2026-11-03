# Changelog

What changed in each release of MetaScrub, newest first. From 1.0, versions follow [Semantic Versioning](https://semver.org); the [README](README.md#stability) says what that promises.

## Unreleased

Everything since 0.3.0. It will be released as 1.0.0.

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
