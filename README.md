# MetaScrub

> Remove hidden metadata from your files.

MetaScrub is a cross-platform command-line tool that inspects and removes hidden metadata (location, device, author, dates, download sources) from files and folders, so you can share them without leaking personal information.

**Status:** early development. `inspect`, `clean` and `verify` work for JPEG, PNG and WebP images and for whole folders (`-r`); more formats are coming.

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
