# MetaScrub

> Remove hidden metadata from your files.

MetaScrub is a cross-platform command-line tool that inspects and removes hidden metadata (location, device, author, dates, download sources) from files and folders, so you can share them without leaking personal information.

**Status:** early development. `inspect`, `clean` and `verify` work for JPEG, PNG and WebP images, Word, Excel, PowerPoint and PDF documents, and whole folders (`-r`); more formats are coming.

## Install

Download the archive for your system from the [releases page](https://github.com/joaopedro17/metascrub/releases), extract it and put `metascrub` (`metascrub.exe` on Windows) somewhere on your `PATH`. Each release lists SHA-256 checksums in `SHA256SUMS`.

The binaries aren't signed yet, so the first run needs one extra step:

- **macOS:** `xattr -d com.apple.quarantine metascrub`, or allow it in System Settings → Privacy & Security.
- **Windows:** SmartScreen may warn about an unknown publisher; choose *More info* → *Run anyway*.

Or build it with Rust:

```bash
cargo install --git https://github.com/joaopedro17/metascrub --tag v0.3.0 --locked metascrub-cli
```

## Usage

```bash
metascrub inspect photo.jpg            # list hidden metadata, change nothing
metascrub clean photo.jpg              # write photo.clean.jpg next to it
metascrub clean -r photos -o shared    # clean a folder into shared/photos/…
metascrub clean --in-place photo.jpg   # replace the original
metascrub clean --dry-run -r photos    # show what would be removed
metascrub verify -r shared             # check nothing removable is left
metascrub clean --accept-changes report.docx   # accept tracked changes, then clean
metascrub clean -r --in-place --remove-junk --rename photos   # also tidy names and junk
```

Originals are never changed unless you use `--in-place`. Existing files are never overwritten: a second output becomes `photo.clean-2.jpg`. Folders need `-r`; symbolic links aren't followed, and files that look already cleaned (`*.clean.*`) are skipped. With `-o`, files MetaScrub can't read are copied too, so the folder is complete, and reported as not cleaned.

Reports hide metadata values so they're safe to share; `--show-values` shows them. `--json` prints machine-readable output, `-v` shows where in the file each finding is, and `-q` prints only problems and the summary.

### What is removed and what is kept

| Format | Removed | Kept |
|---|---|---|
| JPEG | EXIF (GPS, camera, serial numbers, dates, names), thumbnails, XMP, IPTC and Photoshop data, comments, MPF secondary images, data after the image (such as Motion Photo videos), unknown segments | Image data, JFIF and Adobe color information |
| PNG | Text chunks, EXIF, XMP, timestamps, unknown chunks, data after the image | Image data, transparency, gamma and color chunks, HDR chunks, APNG animation |
| WebP | EXIF, XMP, unknown chunks, data after the image | Image data, transparency, animation |
| PDF | Document information (author, title, dates, software), XMP, the document ID (replaced by one computed from the cleaned file), page thumbnails, application data, comments, attachments, earlier versions left by incremental saves, unused objects, data after the end | Pages and their content; photos inside are cleaned like JPEG |
| Word, Excel, PowerPoint | Document properties (author, company, dates, custom properties), thumbnail, revision and document IDs, template and folder paths, printer settings, people lists, sensitivity labels, comments, unknown parts, ZIP dates | The document's content; images inside are cleaned like the formats above |

By default, two things that affect how the image looks are kept:

- **Rotation:** a rotated photo keeps a minimal EXIF with only the Orientation tag, so it doesn't display sideways. `--strip-orientation` removes it.
- **Color profile:** kept only if it's a standard one (sRGB, Display P3, Adobe RGB), which reveals nothing. Other profiles can identify a calibrated monitor and are removed; `--keep color-profile` keeps them.

### Office documents

`.docx`, `.xlsx`, `.pptx` and their macro-enabled and template variants. Some things in a document are content you may want, so they get a choice or a warning instead of being removed silently:

- **Comments** are removed with their markers. `--keep comments` keeps them, with every author shown as "Author" and the dates removed (or reset to 1980 where PowerPoint requires one).
- **Tracked changes** in Word can hide deleted text, and every way of removing them changes the text, so a file with tracked changes isn't cleaned until you choose: `--accept-changes` keeps the text as it reads now, `--reject-changes` goes back to the text before the changes. Either way the history and the reviewers' names are removed.
- **Embedded documents** (such as the workbook behind a chart) are cleaned with the same rules. Other embedded files (OLE objects, PDFs) and **macros** are kept and reported, since MetaScrub can't look inside them.
- **Links to other workbooks** keep only the file name, not the folder it was in.
- **Hidden content** (hidden text, hidden sheets or slides, speaker notes) is reported but left alone: it's part of the document.

### PDF documents

Every cleaned PDF is written anew from what the document still uses, so text from earlier versions saved at the end of the file can't survive.

- **Comments** (sticky notes, highlights, drawings, stamps) are removed. `--keep comments` keeps them, with every author shown as "Author" and the dates removed. Links and form fields are never touched.
- **Attachments** are removed. `--keep attachments` keeps them, cleaned with the same rules when MetaScrub can read them (a photo or Word document attached to a PDF), with only their file name and no dates.
- **Scripts** (JavaScript) and **filled-in form fields** are reported but left alone.
- **Encrypted** PDFs are skipped, since MetaScrub never asks for passwords. **Digitally signed** PDFs are refused: rewriting them would break the signature.

### Files and folders

Some information isn't inside a file at all, but stored next to it by the operating system or left in its folder:

- **Download addresses and tags.** Browsers record where a file was downloaded from: in extended attributes on Linux and macOS (`user.xdg.origin.url`, `com.apple.metadata:kMDItemWhereFroms`, `com.apple.quarantine`), and in the `Zone.Identifier` stream on Windows. Finder tags and comments live there too. MetaScrub reports them for files and folders. Its outputs are new files, so they never carry them; with `--in-place` they're removed, even from files MetaScrub can't read (whose content stays as it is).
- **Dates.** Output files and folders get the date 1980-01-01, and keep the original's permissions. With `--in-place -r`, so do the cleaned folder and the folders in it; with `-o`, the folders created in the output (not the output folder itself). `--keep file-times` leaves the current date instead. Cleaning next to the originals leaves folders alone.
- **Junk files** that the operating system leaves in folders (`.DS_Store`, `Thumbs.db`, `desktop.ini`, `._` files, `__MACOSX` folders) can hold thumbnails, file names and download addresses. They're reported; `--remove-junk` deletes them with `--in-place`, or leaves them out of the `-o` copy.
- **File names** like `IMG_20261102_143512.jpg`, `PXL_…`, `Screenshot 2026-11-02 …` or `WhatsApp Image …` show when a file was made and on which phone or with which app. They're reported; `--rename` gives such files a neutral name: `image-001.jpg`, `video-001.mp4`, `document-001.pdf` or `file-001.zip`, numbered per folder. Names without a date or a known camera or app pattern stay as they are.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (skipped and copied files aren't failures) |
| 1 | Some files failed |
| 2 | Invalid usage |
| 3 | All files failed |
| 4 | `verify` found metadata that cleaning would remove |
| 130 | Stopped with Ctrl+C |

## Limitations

MetaScrub removes **hidden metadata**. It does not:

- Remove what is visible or audible in the file itself: faces, reflections, street signs, landmarks, text in the image.
- Remove invisible watermarks or steganography, or defeat camera sensor fingerprinting.
- Guarantee anonymity.
- Securely delete originals. With `--in-place`, the original bytes may still be recoverable from the disk, and other hard links to the file keep the original.
- Clean files it can't read: they're copied or skipped, never silently passed off as clean. This version reads JPEG, PNG, WebP, Office and PDF documents; HEIC, archives and video are planned.
- Clean encrypted or digitally signed PDFs. They're skipped or refused and reported.
- Clean old-format (`.doc`, `.xls`, `.ppt`) or password-protected Office files, Excel workbooks with change history (shared workbooks), or documents linked to files outside them (linked images or objects). These are skipped or refused and reported.
- Remove what documents contain as content: text, hidden text, speaker notes, cached values from linked workbooks, form values, scripts, or what's inside macros and embedded files it can't read.
- Keep HDR in "Ultra HDR" photos: their gain map is a hidden image and is removed, so the photo shows in standard range.
- Set every date the file system keeps. On Linux, a file's change time (ctime) and creation time can't be set by normal programs.
- Remove `com.apple.provenance` on macOS: the system adds it to files written by downloaded apps (MetaScrub included) and doesn't allow removing it. It's an ID, not a description of you or the file, so it isn't reported either.
- Rename folders, or files with names you chose yourself, even if they reveal something.

## Development

Common tasks are in the [`justfile`](justfile). Install [just](https://github.com/casey/just) once:

```bash
cargo install just --locked
```

Then run `just` to list the commands. The main ones:

| Command | What it does |
|---|---|
| `just check` | Everything CI checks: formatting, lints, tests, dependency policy |
| `just test` | All tests (needs [exiftool](https://exiftool.org), as in CI; when installed, [LibreOffice](https://www.libreoffice.org) checks cleaned Office files still open, and [qpdf](https://qpdf.sourceforge.io) and poppler's `pdftoppm` that cleaned PDFs are valid and render) |
| `just fmt` | Format the code |
| `just run inspect photo.jpg` | Run MetaScrub from the source |
| `just fixtures` | Regenerate the test fixtures and the expected test output |
| `just fuzz jpeg 600` | Fuzz a parser for 10 minutes (Linux/macOS, nightly Rust) |

`just check` also needs [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) (`cargo install cargo-deny --locked`).

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
