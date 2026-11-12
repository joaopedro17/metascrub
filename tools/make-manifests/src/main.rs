//! Fills in the package manager files in `packaging/` for a published
//! release: `make-manifests <version> <SHA256SUMS> <output folder>`.
//! The output folders match the layout of each repository they go to.

use std::path::Path;
use std::process::ExitCode;

/// Each template and where it goes in the output folder.
const TEMPLATES: [(&str, &str); 4] = [
    (
        "homebrew/Formula/metascrub.rb",
        include_str!("../../../packaging/homebrew/metascrub.rb"),
    ),
    (
        "scoop/bucket/metascrub.json",
        include_str!("../../../packaging/scoop/metascrub.json"),
    ),
    (
        "aur/metascrub-bin/PKGBUILD",
        include_str!("../../../packaging/aur/PKGBUILD"),
    ),
    (
        "aur/metascrub-bin/.SRCINFO",
        include_str!("../../../packaging/aur/.SRCINFO"),
    ),
];

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [version, sums, out] = args.as_slice() else {
        eprintln!("usage: make-manifests <version> <SHA256SUMS> <output folder>");
        return ExitCode::FAILURE;
    };
    match run(version, Path::new(sums), Path::new(out)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("make-manifests: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(version: &str, sums: &Path, out: &Path) -> Result<(), String> {
    let sums = std::fs::read_to_string(sums).map_err(|e| format!("{}: {e}", sums.display()))?;
    for (path, template) in TEMPLATES {
        let text = render(template, version, &sums).map_err(|e| format!("{path}: {e}"))?;
        let path = out.join(path);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("{}", path.display());
    }
    Ok(())
}

/// The template with `{{version}}` and every `{{sha256 <target>}}` (the
/// checksum of that target's archive in `sums`) replaced.
fn render(template: &str, version: &str, sums: &str) -> Result<String, String> {
    let valid = version.split('.').count() == 3
        && version
            .split('.')
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if !valid {
        return Err(format!("'{version}' isn't a version like 1.2.0"));
    }
    let mut text = template.replace("{{version}}", version);
    while let Some(start) = text.find("{{sha256 ") {
        let rest = text.get(start + "{{sha256 ".len()..).unwrap_or_default();
        let target = rest
            .split_once("}}")
            .map(|(target, _)| target.to_owned())
            .ok_or("a placeholder isn't closed")?;
        let extension = if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        };
        let archive = format!("metascrub-v{version}-{target}.{extension}");
        let hash = sums
            .lines()
            .filter_map(|line| line.split_once(char::is_whitespace))
            .find(|(_, name)| name.trim_start().trim_start_matches('*') == archive)
            .map(|(hash, _)| hash)
            .filter(|hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| format!("no checksum for {archive}"))?;
        text.replace_range(start..start + "{{sha256 ".len() + target.len() + 2, hash);
    }
    if text.contains("{{") {
        return Err("a placeholder wasn't filled in".into());
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TARGETS: [&str; 6] = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "aarch64-unknown-linux-musl",
        "x86_64-unknown-linux-musl",
        "aarch64-pc-windows-msvc",
        "x86_64-pc-windows-msvc",
    ];

    /// A SHA256SUMS like the release's, with a different checksum per file.
    fn sums(version: &str) -> String {
        TARGETS
            .iter()
            .enumerate()
            .flat_map(|(i, target)| {
                let ext = if target.contains("windows") {
                    "zip"
                } else {
                    "tar.gz"
                };
                [
                    format!("{}  metascrub-v{version}-{target}.cdx.json", "f".repeat(64)),
                    format!("{i:064x}  metascrub-v{version}-{target}.{ext}"),
                ]
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn every_template_is_filled_in() {
        for (path, template) in TEMPLATES {
            let text = render(template, "1.2.0", &sums("1.2.0")).unwrap();
            assert!(!text.contains("{{"), "{path}");
            assert!(text.contains("1.2.0"), "{path}");
            assert!(
                !text.contains(&"f".repeat(64)),
                "{path}: an SBOM's checksum"
            );
        }
    }

    #[test]
    fn each_archive_gets_its_own_checksum() {
        let (_, template) = TEMPLATES[0];
        let text = render(template, "1.2.0", &sums("1.2.0")).unwrap();
        for (i, target) in TARGETS.iter().enumerate().take(4) {
            let url = format!("metascrub-v1.2.0-{target}.tar.gz\"\n");
            let at = text.find(&url).unwrap();
            let next = text.get(at..).unwrap().lines().nth(1).unwrap();
            assert_eq!(next.trim(), format!("sha256 \"{i:064x}\""), "{target}");
        }
    }

    #[test]
    fn scoop_manifest_is_json() {
        let (_, template) = TEMPLATES[1];
        let text = render(template, "1.2.0", &sums("1.2.0")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(json["version"], "1.2.0");
        assert_eq!(json["architecture"]["arm64"]["hash"], format!("{:064x}", 4));
    }

    #[test]
    fn missing_archives_and_bad_versions_are_refused() {
        let (_, template) = TEMPLATES[2];
        let sums = sums("1.2.0");
        assert!(render(template, "1.2.1", &sums).is_err());
        assert!(render(template, "v1.2.0", &sums).is_err());
        assert!(render(template, "1.2", &sums).is_err());
        let without_arm = sums
            .lines()
            .filter(|l| !l.contains("aarch64-unknown-linux-musl.tar.gz"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(render(template, "1.2.0", &without_arm).is_err());
    }
}
