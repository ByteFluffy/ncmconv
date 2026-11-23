#!/bin/sh
# Regenerates Formula/ncmconv.rb from a published release's checksums.
# Usage: scripts/update-formula.sh v0.6.0
set -eu

repo="ByteFluffy/ncmconv"
version=${1:?usage: update-formula.sh <tag, e.g. v0.6.0>}
strip=${version#v}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "https://github.com/$repo/releases/download/$version/checksums.txt" -o "$tmp/checksums.txt" ||
    { echo "error: no checksums.txt for $version (was the release published?)" >&2; exit 1; }

sha() {
    line=$(grep "ncmconv-$version-$1.tar.gz\$" "$tmp/checksums.txt" || true)
    [ -n "$line" ] || { echo "error: $1 missing from checksums.txt" >&2; exit 1; }
    printf '%s' "${line%% *}"
}

formula="Formula/ncmconv.rb"
cat > "$formula" <<RUBY
class Ncmconv < Formula
  desc "Decrypt NetEase .ncm files and convert them to open formats"
  homepage "https://github.com/$repo"
  version "$strip"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/$repo/releases/download/$version/ncmconv-$version-aarch64-apple-darwin.tar.gz"
      sha256 "$(sha aarch64-apple-darwin)"
    else
      url "https://github.com/$repo/releases/download/$version/ncmconv-$version-x86_64-apple-darwin.tar.gz"
      sha256 "$(sha x86_64-apple-darwin)"
    end
  end

  on_linux do
    url "https://github.com/$repo/releases/download/$version/ncmconv-$version-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "$(sha x86_64-unknown-linux-gnu)"
  end

  def install
    bin.install "ncmconv"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ncmconv -version")
  end
end
RUBY

echo "wrote $formula for $version"
git --no-pager diff --stat -- "$formula" || true
