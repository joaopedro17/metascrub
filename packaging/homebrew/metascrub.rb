# Homebrew formula for the tap joaopedro17/homebrew-metascrub (Formula/metascrub.rb).
# Filled in by `just manifests <version>`; don't edit the copy in the tap by hand.
class Metascrub < Formula
  desc "Remove hidden metadata from photos, videos, audio, documents and archives"
  homepage "https://github.com/joaopedro17/metascrub"
  version "{{version}}"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    on_arm do
      url "https://github.com/joaopedro17/metascrub/releases/download/v{{version}}/metascrub-v{{version}}-aarch64-apple-darwin.tar.gz"
      sha256 "{{sha256 aarch64-apple-darwin}}"
    end
    on_intel do
      url "https://github.com/joaopedro17/metascrub/releases/download/v{{version}}/metascrub-v{{version}}-x86_64-apple-darwin.tar.gz"
      sha256 "{{sha256 x86_64-apple-darwin}}"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/joaopedro17/metascrub/releases/download/v{{version}}/metascrub-v{{version}}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "{{sha256 aarch64-unknown-linux-musl}}"
    end
    on_intel do
      url "https://github.com/joaopedro17/metascrub/releases/download/v{{version}}/metascrub-v{{version}}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "{{sha256 x86_64-unknown-linux-musl}}"
    end
  end

  def install
    bin.install "metascrub"
    man1.install Dir["man/*.1"]
    bash_completion.install "completions/metascrub.bash" => "metascrub"
    zsh_completion.install "completions/_metascrub"
    fish_completion.install "completions/metascrub.fish"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/metascrub --version")
    (testpath/"notes.txt").write "hello"
    assert_match "1 skipped", shell_output("#{bin}/metascrub inspect #{testpath}/notes.txt")
  end
end
