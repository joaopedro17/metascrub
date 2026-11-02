# Fuzzing

Fuzz targets feed malformed input to the parsers and cleaners; any panic is a bug. They need the nightly toolchain and `cargo-fuzz`:

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
| `webp` | Each WebP chunk reader and the cleaner; the input is wrapped in a well-formed extended WebP |
| `clean` | Every cleaner: the output must pass verification, and cleaning it again must change nothing |

Crashing inputs are saved in `artifacts/<target>/`.
