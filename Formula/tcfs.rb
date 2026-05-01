# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.4, fe9b7857890c0a4808b06f7439719ca6198231b3cb63f7b606be86eb3fa51eef, 1986394f7deb187d1dc99fd62f29252c6655aaecb1e48718280f0f8bf3419912,
#               6f0d867c16b275e2dd4786b31e932752dd7d02aaa0cb98cca326ae4cc9fd0a51, a05209e3706d19c973405274c4830341d79067027e72fcd6c53d84092222b41c

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.4"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.4/tcfs-0.12.4-macos-aarch64.tar.gz"
      sha256 "fe9b7857890c0a4808b06f7439719ca6198231b3cb63f7b606be86eb3fa51eef"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.4/tcfs-0.12.4-macos-x86_64.tar.gz"
      sha256 "1986394f7deb187d1dc99fd62f29252c6655aaecb1e48718280f0f8bf3419912"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.4/tcfs-0.12.4-linux-aarch64.tar.gz"
      sha256 "a05209e3706d19c973405274c4830341d79067027e72fcd6c53d84092222b41c"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.4/tcfs-0.12.4-linux-x86_64.tar.gz"
      sha256 "6f0d867c16b275e2dd4786b31e932752dd7d02aaa0cb98cca326ae4cc9fd0a51"
    end
  end

  def install
    bin.install "tcfs"
    bin.install "tcfsd"
    bin.install "tcfs-tui"
  end

  service do
    run [opt_bin/"tcfsd", "--config", etc/"tcfs/config.toml"]
    keep_alive true
    log_path var/"log/tcfsd.log"
    error_log_path var/"log/tcfsd.log"
  end

  test do
    assert_match "tcfs", shell_output("#{bin}/tcfs --version")
  end
end
