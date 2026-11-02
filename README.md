# MetaScrub

> Remove hidden metadata from your files.

MetaScrub is a cross-platform command-line tool that inspects and removes hidden metadata (location, device, author, dates, download sources) from files and folders, so you can share them without leaking personal information.

**Status:** early development. `inspect`, `clean` and `verify` work for JPEG, PNG and WebP images and for whole folders (`-r`); more formats are coming.

## Install

Download the archive for your system from the [releases page](https://github.com/joaopedro17/metascrub/releases), extract it and put `metascrub` (`metascrub.exe` on Windows) somewhere on your `PATH`. Each release lists SHA-256 checksums in `SHA256SUMS`.

The binaries aren't signed yet, so the first run needs one extra step:

- **macOS:** `xattr -d com.apple.quarantine metascrub`, or allow it in System Settings → Privacy & Security.
- **Windows:** SmartScreen may warn about an unknown publisher; choose *More info* → *Run anyway*.

Or build it with Rust:

```bash
cargo install --git https://github.com/joaopedro17/metascrub --tag v0.2.0 --locked metascrub-cli
```

## Usage

```bash
metascrub inspect photo.jpg            # list hidden metadata, change nothing
metascrub clean photo.jpg              # write photo.clean.jpg next to it
metascrub clean -r photos -o shared    # clean a folder into shared/photos/…
metascrub clean --in-place photo.jpg   # replace the original
metascrub clean --dry-run -r photos    # show what would be removed
metascrub verify -r shared             # check nothing removable is left
```

Originals are never changed unless you use `--in-place`. Existing files are never overwritten: a second output becomes `photo.clean-2.jpg`. Folders need `-r`; symbolic links aren't followed, and files that look already cleaned (`*.clean.*`) are skipped. With `-o`, files MetaScrub can't read are copied too, so the folder is complete, and reported as not cleaned.

Reports hide metadata values so they're safe to share; `--show-values` shows them. `--json` prints machine-readable output, `-v` shows where in the file each finding is, and `-q` prints only problems and the summary.

### What is removed and what is kept

| Format | Removed | Kept |
|---|---|---|
| JPEG | EXIF (GPS, camera, serial numbers, dates, names), thumbnails, XMP, IPTC and Photoshop data, comments, MPF secondary images, data after the image (such as Motion Photo videos), unknown segments | Image data, JFIF and Adobe color information |
| PNG | Text chunks, EXIF, XMP, timestamps, unknown chunks, data after the image | Image data, transparency, gamma and color chunks, HDR chunks, APNG animation |
| WebP | EXIF, XMP, unknown chunks, data after the image | Image data, transparency, animation |

By default, two things that affect how the image looks are kept:

- **Rotation:** a rotated photo keeps a minimal EXIF with only the Orientation tag, so it doesn't display sideways. `--strip-orientation` removes it.
- **Color profile:** kept only if it's a standard one (sRGB, Display P3, Adobe RGB), which reveals nothing. Other profiles can identify a calibrated monitor and are removed; `--keep color-profile` keeps them.

Output files get the date 1980-01-01 and the original's permissions. `--keep file-times` leaves the current date instead.

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
- Clean files it can't read: they're copied or skipped, never silently passed off as clean. This version reads JPEG, PNG and WebP only; HEIC, Office documents, PDF, archives and video are planned.
- Keep HDR in "Ultra HDR" photos: their gain map is a hidden image and is removed, so the photo shows in standard range.
- Report filesystem metadata yet, such as extended attributes or Windows alternate data streams that hold a download URL. Outputs are always written as new files, so they don't carry these. On Linux, a file's change time (ctime) can't be set by normal programs.

## Development

Common tasks are in the [`justfile`](justfile). Install [just](https://github.com/casey/just) once:

```bash
cargo install just --locked
```

Then run `just` to list the commands. The main ones:

| Command | What it does |
|---|---|
| `just check` | Everything CI checks: formatting, lints, tests, dependency policy |
| `just test` | All tests (needs [exiftool](https://exiftool.org), as in CI) |
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
