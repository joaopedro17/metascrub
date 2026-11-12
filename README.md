# MetaScrub

> Remove hidden metadata from your files.

MetaScrub is a cross-platform command-line tool that inspects and removes hidden metadata (location, device, author, dates, download sources) from files and folders, so you can share them without leaking personal information.

**Status:** early development. `inspect`, `clean` and `verify` work for JPEG, PNG, WebP, HEIC and AVIF images, MP4 and QuickTime videos, M4A, MP3, FLAC, Ogg Vorbis, Opus and WAV audio, Word, Excel, PowerPoint, OpenDocument (LibreOffice) and PDF documents, ZIP, TAR and `.tar.gz` archives, and whole folders (`-r`).

## Install

Download the archive for your system from the [releases page](https://github.com/joaopedro17/metascrub/releases), extract it and put `metascrub` (`metascrub.exe` on Windows) somewhere on your `PATH`. Each release lists SHA-256 checksums in `SHA256SUMS`.

To check that a download was built by this project's GitHub workflow from its source, and not changed since, use the [GitHub CLI](https://cli.github.com):

```bash
gh attestation verify metascrub-v1.1.1-x86_64-unknown-linux-musl.tar.gz --repo joaopedro17/metascrub
```

Next to each archive, `.cdx.json` is its software bill of materials: every library in that binary, in [CycloneDX](https://cyclonedx.org) format. It's attested too (add `--predicate-type https://cyclonedx.org/bom` to check it).

The binaries aren't signed with an Apple or Microsoft certificate, so the first run needs one extra step:

- **macOS:** `xattr -d com.apple.quarantine metascrub`, or allow it in System Settings → Privacy & Security.
- **Windows:** SmartScreen may warn about an unknown publisher; choose *More info* → *Run anyway*.

Or build it with Rust:

```bash
cargo install --git https://github.com/joaopedro17/metascrub --tag v1.1.1 --locked metascrub-cli
```

### Tab completion and manual pages

The archives include completion scripts for Bash, Zsh, Fish and PowerShell in `completions/`, and manual pages in `man/`. `metascrub completions <shell>` and `metascrub man [command]` print the same files, so they also work when MetaScrub was installed another way. To install them:

| Shell | Command |
|---|---|
| Bash | `metascrub completions bash > ~/.local/share/bash-completion/completions/metascrub` |
| Zsh | `metascrub completions zsh > ~/.zfunc/_metascrub`, with `fpath+=~/.zfunc` before `compinit` in `~/.zshrc` |
| Fish | `metascrub completions fish > ~/.config/fish/completions/metascrub.fish` |
| PowerShell | Add `metascrub completions powershell \| Out-String \| Invoke-Expression` to your profile (`notepad $PROFILE`) |

For the manual pages on Linux or macOS, copy `man/*.1` into `~/.local/share/man/man1/` (or `/usr/local/share/man/man1/`); then `man metascrub` and `man metascrub-clean` work.

## Usage

```bash
metascrub inspect photo.jpg            # list hidden metadata, change nothing
metascrub clean photo.jpg              # write photo.clean.jpg next to it
metascrub clean -r photos -o shared    # clean a folder into shared/photos/…
metascrub clean --in-place photo.jpg   # replace the original
metascrub clean holiday.zip            # every file inside cleaned, in holiday.clean.zip
metascrub clean --dry-run -r photos    # show what would be removed
metascrub verify -r shared             # check nothing removable is left
metascrub clean --accept-changes report.docx   # accept tracked changes, then clean
metascrub clean -r --in-place --remove-junk --rename photos   # also tidy names and junk
metascrub clean --checksums photo.jpg  # also show the SHA-256 of the original and the copy
```

Originals are never changed unless you use `--in-place`. Existing files are never overwritten: a second output becomes `photo.clean-2.jpg`. Folders need `-r`; symbolic links aren't followed, and files that look already cleaned (`*.clean.*`) are skipped. With `-o`, files MetaScrub can't read are copied too, so the folder is complete, and reported as not cleaned.

Before a cleaned file is kept, MetaScrub reads it back the way `verify` does. If anything it should have removed is still there, the file isn't written (or the original isn't replaced) and it's reported as failed: that would be a bug in MetaScrub, worth [reporting](https://github.com/joaopedro17/metascrub/issues).

Reports hide metadata values so they're safe to share; `--show-values` shows them. `--json` prints machine-readable output, `-v` shows where in the file each finding is, and `-q` prints only problems and the summary. `--checksums` adds the SHA-256 of each file read and each file written, so you can show exactly which file you shared.

### What is removed and what is kept

| Format | Removed | Kept |
|---|---|---|
| JPEG | EXIF (GPS, camera, serial numbers, dates, names), thumbnails, XMP, IPTC and Photoshop data, comments, MPF secondary images, data after the image (such as Motion Photo videos), unknown segments | Image data, JFIF and Adobe color information |
| PNG | Text chunks, EXIF, XMP, timestamps, unknown chunks, data after the image | Image data, transparency, gamma and color chunks, HDR chunks, APNG animation |
| WebP | EXIF, XMP, unknown chunks, data after the image | Image data, transparency, animation |
| PDF | Document information (author, title, dates, software), XMP, the document ID (replaced by one computed from the cleaned file), page thumbnails, application data, comments, attachments, earlier versions left by incremental saves, unused objects, data after the end | Pages and their content; photos inside are cleaned like JPEG |
| Word, Excel, PowerPoint | Document properties (author, company, dates, custom properties), thumbnail, revision and document IDs, template and folder paths, printer settings, people lists, sensitivity labels, comments, unknown parts, ZIP dates | The document's content; images inside are cleaned like the formats above |
| OpenDocument (ODT, ODS, ODP, ODG) | Document properties (`meta.xml`: author, dates, editing time, template path, custom properties), thumbnail, view and printer settings, editing-session IDs, file paths in settings, comments, files not in the manifest, ZIP dates | The document's content and layout settings; images inside are cleaned like the formats above |
| HEIC, AVIF | EXIF (rebuilt with only the orientation), XMP, data after the image | Image data, previews, depth and HDR gain maps (see below) |
| MP4, MOV, M4A | Location, device make and model, software and encoder names, titles, names, comments, cover art and other tags (QuickTime user data, Apple and iTunes metadata, XMP), dates in the headers (set to zero), timecodes and GPS or sensor tracks (zeroed), leftover data in free space, data after the end | Picture and sound |
| MP3 | ID3v2 and ID3v1 tags (title, artist, album, dates, comments, lyrics, cover art, loudness levels and every other tag), APE and Lyrics3 tags | The sound, byte for byte, and the first frame that holds the length and seek table (with the encoder's name) |
| FLAC | Vorbis comments (title, artist, dates, comments, loudness levels and every other tag, and the encoder's name), pictures, application data, padding, ID3 and APE tags added before or after | The sound, byte for byte, the stream description, seek table and cue sheet |
| Ogg Vorbis, Opus | The comment header (title, artist, dates, comments, cover art, loudness levels and every other tag, and the encoder's name, left empty), ID3 and APE tags added after | The sound, byte for byte, the codec's other headers, and Opus's volume adjustment |
| WAV | Tags (`LIST INFO`), names and notes on cue points, Broadcast WAV metadata (description, originator, reference, dates, time of day, UMID, loudness, coding history), iXML, XMP, ID3, the sampler's maker and model and SMPTE time (set to zero), any other chunk, data after the end | The sound, byte for byte, its format, cue points, sampler loops and instrument settings |
| ZIP, TAR, gzip | Dates, owner names and IDs, permissions (reset to 644, or 755 for folders and programs), comments, extra fields and PAX records (including saved download addresses), gzip's original file name, junk files | Every file inside, cleaned like the formats above; files MetaScrub can't read are kept as they are |

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

### OpenDocument files

`.odt`, `.ods`, `.odp`, `.odg` and their templates, as LibreOffice and OpenOffice write them. The same choices as for Office documents apply:

- **Comments** are removed. `--keep comments` keeps them, with every author shown as "Author" and the dates removed.
- **Settings** stay where the layout depends on them (documents converted from Word need them); how the document was last viewed, the printer and its driver data go.
- **Embedded objects** (charts) are cleaned with the same rules; pictures like the formats above. **Macros** (Basic, scripts) are kept and reported.
- **Hidden content** (hidden text, sheets and slides, speaker notes) is reported but left alone.
- **Tracked changes:** files with tracked changes are refused for now. Accept or reject them in LibreOffice first (Edit ▸ Track Changes ▸ Manage).
- **Signed** files are refused (cleaning would break the signature), and **password-protected** ones skipped.

### PDF documents

Every cleaned PDF is written anew from what the document still uses, so text from earlier versions saved at the end of the file can't survive.

- **Comments** (sticky notes, highlights, drawings, stamps) are removed. `--keep comments` keeps them, with every author shown as "Author" and the dates removed. Links and form fields are never touched.
- **Attachments** are removed. `--keep attachments` keeps them, cleaned with the same rules when MetaScrub can read them (a photo or Word document attached to a PDF), with only their file name and no dates.
- **Scripts** (JavaScript) and **filled-in form fields** are reported but left alone.
- **Encrypted** PDFs are skipped, since MetaScrub never asks for passwords. **Digitally signed** PDFs are refused: rewriting them would break the signature.

### Videos, music and HEIC photos

MP4, QuickTime (`.mov`), M4A and HEIC/AVIF files are cleaned in place: nothing is moved, so every video frame and image stays exactly where it was. Metadata blocks become empty space of the same size, dates and recorded data are set to zero, and the photo's EXIF and XMP are overwritten at their own size.

- **GPS and sensor tracks** that some cameras record next to the picture (GoPro, DJI, Google's camera motion track, Apple's timed metadata) are zeroed; the video still plays. Data tracks MetaScrub doesn't recognize are reported but left alone, and a video split into fragments with such a track is refused.
- **Subtitles and chapter titles** aren't checked: they're reported when a video has them.
- **Previews, depth maps and HDR gain maps** in HEIC and AVIF photos are kept: they show only what the photo shows, and removing them would break how the photo displays. They're listed as low risk.
- **Music** loses its tags too: title, artist, album and cover art are removed along with everything else.
- 3GP videos, camera raw files (`.cr3`) and other formats that share this structure aren't cleaned.

MP3, FLAC, Ogg and WAV files keep exactly their sound: only the tags are removed, so it plays exactly as before. In Ogg files the tags sit in a header every file needs, so it's replaced with an empty one; the pages of sound get new page numbers and checksums, but the sound in them isn't touched. MetaScrub checks that everything between the tags is sound (for FLAC and Ogg, through checksums); a file with other data among its frames or pages is refused, since that data could hold anything, and so is an MP3 with less than three frames of sound. Ogg files with more than one stream (several tracks in one file, or one after another) and Ogg files holding FLAC, Speex or video aren't cleaned. WAV files over 4 GB (RF64) and WAV files with object audio (ADM, as in Dolby Atmos masters) are refused.

### Archives

ZIP, TAR and gzip-compressed files (`.tar.gz`, `.tgz`, `.gz`) are treated like a folder inside a file: every file in them is cleaned with the same rules, and the result is a new archive. Archives inside archives are cleaned too, up to 3 levels deep.

- **Files MetaScrub can't read** (a text file, a damaged photo, an encrypted or too deeply nested archive) are kept exactly as they are and listed as possibly still containing hidden information.
- **Junk files** inside (`__MACOSX/`, `.DS_Store`, `._` files, `Thumbs.db`) are always removed: the cleaned archive is a new file, so nothing of yours is deleted.
- **Symbolic links** inside are kept, never followed, and listed with their target.
- **Safety:** an archive may unpack to at most 4 GB in total, archives inside it included, with at most 10,000 files, and no file may unpack to more than 100 times its compressed size (zip bombs). Archives with paths that point outside them (`../`) are refused; encrypted ones are skipped, since MetaScrub never asks for passwords. Archives of any size are processed a file at a time, using temporary files rather than memory for large ones.
- **Office and OpenDocument files** inside archives are cleaned like files on disk. **Apps, e-books and extensions** are ZIP files too (`.jar`, `.apk`, `.epub`, `.vsix`, …), but they're often signed and depend on their exact content, so they're not treated as archives.

### Files and folders

Some information isn't inside a file at all, but stored next to it by the operating system or left in its folder:

- **Download addresses and tags.** Browsers record where a file was downloaded from: in extended attributes on Linux and macOS (`user.xdg.origin.url`, `com.apple.metadata:kMDItemWhereFroms`, `com.apple.quarantine`), and in the `Zone.Identifier` stream on Windows. Finder tags and comments live there too. MetaScrub reports them for files and folders. Its outputs are new files, so they never carry them; with `--in-place` they're removed, even from files MetaScrub can't read (whose content stays as it is).
- **Dates.** Output files and folders get the date 1980-01-01, and keep the original's permissions. With `--in-place -r`, so do the cleaned folder and the folders in it; with `-o`, the folders created in the output (not the output folder itself). `--keep file-times` leaves the current date instead. Cleaning next to the originals leaves folders alone.
- **Junk files** that the operating system leaves in folders (`.DS_Store`, `Thumbs.db`, `desktop.ini`, `._` files, `__MACOSX` folders) can hold thumbnails, file names and download addresses. They're reported; `--remove-junk` deletes them with `--in-place`, or leaves them out of the `-o` copy.
- **File names** like `IMG_20261102_143512.jpg`, `PXL_…`, `Screenshot 2026-11-02 …` or `WhatsApp Image …` show when a file was made and on which phone or with which app. They're reported; `--rename` gives such files a neutral name: `image-001.jpg`, `video-001.mp4`, `audio-001.m4a`, `document-001.pdf` or `file-001.zip`, numbered per folder. Names without a date or a known camera or app pattern stay as they are.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success (skipped and copied files aren't failures) |
| 1 | Some files failed |
| 2 | Invalid usage |
| 3 | All files failed |
| 4 | `verify` found metadata that cleaning would remove |
| 130 | Stopped with Ctrl+C |

### Speed and memory

Measured with the Linux download of 1.1.1 on a 24-thread PC (WSL) with an SSD, three runs each:

| Task | Time | Peak memory |
|---|---|---|
| Inspect 5,100 small files of every supported type in 100 folders | 1.3 s | 20 MB |
| Clean the same folders | 2.1–2.2 s | 24 MB |
| Clean 200 phone photos (8 MB each, 1.6 GB) | 1.1–1.4 s | 34 MB |
| Clean a 2 GB video | 0.9–3 s | 2 MB |
| Clean a 600 MB ZIP of 75 phone photos | 16–20 s | 42 MB |
| Clean four 400 MB PDFs | 13–15 s | 2 GB |

The Linux download is built with musl so it runs on any distribution; musl handles memory more slowly when several files are processed at once. Built from source on Linux (`cargo install`), the small files and photos are up to eight times faster.

Videos, music and archives are processed from disk, so their size barely changes the memory used. Images, PDFs and Office files are read whole, and a PDF can take several times its size while it's rewritten; MetaScrub processes up to 4 files at once, but large ones wait their turn so they stay within about 2 GB together. A single file larger than that runs alone.

### Stability

From 1.0, MetaScrub follows [Semantic Versioning](https://semver.org). Within 1.x, scripts that use it keep working:

- **Commands, options and exit codes** keep their names and meaning. New ones may be added.
- **The JSON report** (`--json`) keeps its fields, their meaning and `"schema_version": 1`. New fields and new values (formats, kinds of findings, outcomes, reasons) may be added in any 1.x release, so a script should ignore fields it doesn't know and treat values it doesn't know as "something else".
- **What is found** may grow: a later release may find, and remove, more than an earlier one, and may judge a finding's risk differently.
- **The text report** is written for people and may be reworded in any release. Scripts should use `--json` and the exit codes, not the text.

Anything else would need 2.0, with a new `schema_version` for JSON changes. Before 1.0, anything could change. [CHANGELOG.md](CHANGELOG.md) lists what changed in each release.

### Reporting a problem

If MetaScrub fails on a file or does something unexpected, run the same command again with `--log metascrub.log` and attach that file to an [issue](https://github.com/joaopedro17/metascrub/issues):

```bash
metascrub clean -r photos -o shared --log metascrub.log
```

The log shows what MetaScrub did with each file: the format it found, each step and how long it took, and the full error. It **never contains metadata values**, and files are numbered (`file 12`) instead of named, so it's safe to share. With `--show-values` it names the files too; check it before you share it. An existing file is never overwritten: a second log becomes `metascrub-2.log`. Nothing is logged without `--log`. For debugging, `METASCRUB_LOG=debug` prints the same messages to the terminal.

## Limitations

MetaScrub removes **hidden metadata**. It does not:

- Remove what is visible or audible in the file itself: faces, reflections, street signs, landmarks, text in the image.
- Remove invisible watermarks or steganography, or defeat camera sensor fingerprinting.
- Guarantee anonymity.
- Securely delete originals. With `--in-place`, the original bytes may still be recoverable from the disk, and other hard links to the file keep the original.
- Clean files it can't read: they're copied or skipped, never silently passed off as clean. This version reads JPEG, PNG, WebP, HEIC and AVIF images, MP4, MOV, M4A, MP3, FLAC, Ogg Vorbis, Opus and WAV files, Office, OpenDocument and PDF documents, and ZIP, TAR and gzip archives.
- Check subtitles, chapter titles or what's in the sound itself, or clean MKV, WebM, AVI or 3GP videos.
- Clean 7z or RAR archives, ZIP files compressed with methods other than Deflate (such as LZMA or Zstandard), or TAR archives with sparse files or device files. These are refused and reported.
- Clean encrypted or digitally signed PDFs. They're skipped or refused and reported.
- Clean old-format (`.doc`, `.xls`, `.ppt`) or password-protected Office files, Excel workbooks with change history (shared workbooks), or documents linked to files outside them (linked images or objects). These are skipped or refused and reported.
- Clean OpenDocument files with tracked changes, signed or password-protected ones, or flat OpenDocument files (`.fodt`). These are refused or skipped and reported.
- Remove what documents contain as content: text, hidden text, speaker notes, cached values from linked workbooks, form values, scripts, or what's inside macros and embedded files it can't read.
- Keep HDR in "Ultra HDR" photos: their gain map is a hidden image and is removed, so the photo shows in standard range.
- Set every date the file system keeps. On Linux, a file's change time (ctime) and creation time can't be set by normal programs.
- Remove `com.apple.provenance` on macOS: the system adds it to files written by downloaded apps (MetaScrub included) and doesn't allow removing it. It's an ID, not a description of you or the file, so it isn't reported either.
- Rename folders, or files with names you chose yourself, even if they reveal something. Names of files inside archives are never changed.

## Development

Common tasks are in the [`justfile`](justfile). Install [just](https://github.com/casey/just) once:

```bash
cargo install just --locked
```

Then run `just` to list the commands. The main ones:

| Command | What it does |
|---|---|
| `just check` | Everything CI checks: formatting, lints, tests, dependency policy |
| `just test` | All tests (needs [exiftool](https://exiftool.org), as in CI; when installed, [LibreOffice](https://www.libreoffice.org) checks cleaned Office files still open, [qpdf](https://qpdf.sourceforge.io) and poppler's `pdftoppm` that cleaned PDFs are valid and render, and [ffmpeg](https://ffmpeg.org) and libheif's `heif-convert` that cleaned videos and photos still play and decode) |
| `just fmt` | Format the code |
| `just run inspect photo.jpg` | Run MetaScrub from the source |
| `just fixtures` | Regenerate the test fixtures and the expected test output (needs ffmpeg, and libheif's `heif-enc` with its x265 and aom plugins) |
| `just fuzz jpeg 600` | Fuzz a parser for 10 minutes (Linux/macOS, nightly Rust) |

`just check` also needs [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) (`cargo install cargo-deny --locked`).

## Support

MetaScrub is free and always will be. If it's kept something private for you
and you feel like saying thanks, you can [buy me a coffee](https://buymeacoffee.com/joaopedro.luz17)
— entirely optional, hugely appreciated.

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or <https://opensource.org/licenses/MIT>)

at your option.

### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
