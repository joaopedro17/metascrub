# Package managers

Templates for the package managers that install MetaScrub's release downloads. `{{version}}` and `{{sha256 <target>}}` are filled in by:

```bash
just manifests 1.2.0
```

It downloads that release's `SHA256SUMS` and writes the files into `target/manifests/1.2.0/`, in the layout of the repository each one goes to. So the release must be published first.

| Folder | Goes to |
|---|---|
| `homebrew/Formula/metascrub.rb` | The tap, [joaopedro17/homebrew-metascrub](https://github.com/joaopedro17/homebrew-metascrub) |
| `scoop/bucket/metascrub.json` | The bucket, [joaopedro17/scoop-metascrub](https://github.com/joaopedro17/scoop-metascrub) |
| `aur/metascrub-bin/` (`PKGBUILD`, `.SRCINFO`) | The AUR, `ssh://aur@aur.archlinux.org/metascrub-bin.git` |

crates.io gets the source instead, published with `cargo publish`.

## Setting up (once)

- **Homebrew:** create the public repository `joaopedro17/homebrew-metascrub` with the formula at `Formula/metascrub.rb`. The name must start with `homebrew-`; users then type `brew install joaopedro17/metascrub/metascrub`.
- **Scoop:** create `joaopedro17/scoop-metascrub` from [ScoopInstaller/BucketTemplate](https://github.com/ScoopInstaller/BucketTemplate) ("Use this template") and put the manifest in `bucket/`. The template's Excavator workflow checks for new releases every few hours and updates the manifest itself, using `checkver` and `autoupdate` (the hashes come from `SHA256SUMS`).
- **AUR:** create an account on [aur.archlinux.org](https://aur.archlinux.org), add an SSH public key to it, then `git clone ssh://aur@aur.archlinux.org/metascrub-bin.git` (an empty repository the first time), copy both files in, commit and push. You can add your name and e-mail as a `# Maintainer:` line at the top of the `PKGBUILD`.
- **crates.io:** log in with GitHub on [crates.io](https://crates.io), create an API token, then `cargo login`. Both crates are published: `metascrub-core` first, since `metascrub` (the command) needs that exact version.

## Each release

After the release is published on GitHub:

1. `cargo publish -p metascrub-core`, then `cargo publish -p metascrub`, from the release commit on `main`.
2. `just manifests X.Y.Z`.
3. Homebrew: copy `Formula/metascrub.rb` into the tap and push.
4. AUR: copy `PKGBUILD` and `.SRCINFO` into the AUR clone and push. If only the `PKGBUILD` changes between releases, raise `pkgrel` by hand in both files.
5. Scoop updates itself. To do it right away, run the bucket's Excavator workflow from its Actions tab.
