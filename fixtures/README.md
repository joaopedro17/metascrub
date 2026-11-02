# Test fixtures

`generated/` holds synthetic files built from scratch by `tools/make-fixtures` with fictional metadata (Jane Doe, a camera brand that doesn't exist, the Eiffel Tower's coordinates). No file here contains a real person's data.

Regenerate them with:

```bash
cargo run -p make-fixtures
```

Then update the expected test output:

```bash
METASCRUB_BLESS=1 cargo test -p metascrub-cli --test golden
```

Files donated from real devices will go in `donated/`, each with the donor's consent and license recorded in `donated/LICENSES.md`.
