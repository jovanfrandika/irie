#!/usr/bin/env bash
# Usage: scripts/homebrew-formula.sh <tag> <dir-with-sha256-files>
# Prints the Homebrew formula for the given release.
set -euo pipefail
tag="$1"; dir="$2"
sha() { awk '{print $1}' "$dir/irie-$tag-$1.tar.gz.sha256"; }
base="https://github.com/jovanfrandika/irie/releases/download/$tag"
cat <<RUBY
class Irie < Formula
  desc "Spotify client for the terminal: play, visualize, browse, queue and Jam"
  homepage "https://github.com/jovanfrandika/irie"
  license "MIT"

  on_macos do
    on_arm do
      url "$base/irie-$tag-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    end
    on_intel do
      url "$base/irie-$tag-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$base/irie-$tag-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha aarch64-unknown-linux-gnu)"
    end
    on_intel do
      url "$base/irie-$tag-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "$(sha x86_64-unknown-linux-gnu)"
    end
  end

  def install
    bin.install "irie"
    pkgshare.install "config.example.toml"
  end

  def caveats
    <<~EOS
      irie needs a Spotify Premium account. Run \`irie\` and log in through your browser.
      An example config is at:
        #{opt_pkgshare}/config.example.toml
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/irie --version")
  end
end
RUBY
