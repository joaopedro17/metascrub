# Test fixtures

`generated/` holds synthetic files built from scratch by `tools/make-fixtures` with fictional metadata (Jane Doe, a camera brand that doesn't exist, the Eiffel Tower's coordinates). No file here contains a real person's data.

Regenerate them, and the expected test output, with:

```bash
just fixtures
```

Files donated from real devices will go in `donated/`, each with the donor's consent and license recorded in `donated/LICENSES.md`.
