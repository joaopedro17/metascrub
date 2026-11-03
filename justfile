# MetaScrub development commands. Run `just` to list them.

set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# List the available commands
default:
    @just --list

# Run everything CI checks, in the same order
check: fmt-check lint test deny

# Format all code
fmt:
    cargo fmt --all

# Check formatting without changing any file
fmt-check:
    cargo fmt --all --check

# Lint with warnings treated as errors
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Run all tests; like CI, fails if exiftool isn't installed or the
# temporary folder doesn't allow extended attributes
test $METASCRUB_REQUIRE_EXIFTOOL="1" $METASCRUB_REQUIRE_FS_ATTRIBUTES="1":
    cargo test --workspace --locked

# Check dependency licenses, advisories and sources (needs cargo-deny)
deny:
    cargo deny check

# Regenerate the fixtures and the expected test output
fixtures $METASCRUB_BLESS="1":
    cargo run -q -p make-fixtures
    cargo test -q -p metascrub-cli --test golden

# Fuzz a target (inspect, jpeg, png, webp, clean, attributes), seeded with the fixtures; needs nightly and cargo-fuzz, Linux or macOS
[working-directory: 'fuzz']
fuzz target seconds="180":
    mkdir -p corpus/{{target}}
    cargo +nightly fuzz run {{target}} corpus/{{target}} ../fixtures/generated/jpeg ../fixtures/generated/png ../fixtures/generated/webp ../fixtures/generated/office ../fixtures/generated/pdf -- -max_total_time={{seconds}}

# Run MetaScrub, e.g. `just run inspect photo.jpg`
run *args:
    cargo run -q -p metascrub-cli -- {{args}}
