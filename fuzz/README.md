# Fuzzing

Fuzz targets feed malformed input to the parsers; any panic is a bug. They need the nightly toolchain and `cargo-fuzz`:

```bash
rustup toolchain install nightly
cargo +nightly install cargo-fuzz --locked
```

Run a target, here for 10 minutes:

```bash
just fuzz jpeg 600
```

| Target | What it reaches |
|---|---|
| `inspect` | Format detection and every parser |
| `jpeg` | The JPEG parser (input always starts with a JPEG signature) |
| `png` | Each PNG chunk reader; the input is wrapped in a chunk with a valid checksum |

Crashing inputs are saved in `artifacts/<target>/`.
