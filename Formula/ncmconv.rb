class Ncmconv < Formula
  desc "Decrypt NetEase .ncm files and convert them to open formats"
  homepage "https://github.com/ByteFluffy/ncmconv"
  version "0.5.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/ByteFluffy/ncmconv/releases/download/v0.5.0/ncmconv-v0.5.0-aarch64-apple-darwin.tar.gz"
      sha256 "a66d62dc4f2feb04317e9b1316fa7436064523c80a876cd27fe4c19fd9b47054"
    else
      url "https://github.com/ByteFluffy/ncmconv/releases/download/v0.5.0/ncmconv-v0.5.0-x86_64-apple-darwin.tar.gz"
      sha256 "17b5df038c60e5ed8e2bc7640ee012f27a8b48b3c1f6fa8b6dac417ab8aabd85"
    end
  end

  on_linux do
    url "https://github.com/ByteFluffy/ncmconv/releases/download/v0.5.0/ncmconv-v0.5.0-x86_64-unknown-linux-gnu.tar.gz"
    sha256 "03fd56cd57f9e085067f3b3198accc6be9c6cb56cb28681b193e6e897d571161"
  end

  def install
    bin.install "ncmconv"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ncmconv -version")
  end
end
