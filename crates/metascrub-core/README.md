# metascrub-core

The engine of [MetaScrub](https://github.com/joaopedro17/metascrub), a tool that removes hidden metadata (location, names, dates, devices) from photos, videos, audio, documents and archives.

This library is published only because the `metascrub` command needs it. Its API isn't stable: any release may change it. To use MetaScrub, install the command:

```bash
cargo install metascrub --locked
```

Scripts can rely on the command's options, exit codes and `--json` output, which follow [Semantic Versioning](https://github.com/joaopedro17/metascrub#stability).

Licensed under either of the MIT license or the Apache License, Version 2.0, at your option.
