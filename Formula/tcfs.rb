# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.12, ac1cc0fb068361779fb6bf0336579487b2dc94a4a4db584bbe7e431548395c98, 5347d79f201ef0be85c58ef30134f02fb335c1493eb8c2e671f2e7474a2940b0,
#               4a1d4c8f41ccd2b8f0430159277ef14a449de1b22740b7a608c4e2451a6d065b, 5cd7b649969f333042cc119532b846e3ba3c94499cad915474f8e2ee2af651d7

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.12"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.12/tcfs-0.12.12-macos-aarch64.tar.gz"
      sha256 "ac1cc0fb068361779fb6bf0336579487b2dc94a4a4db584bbe7e431548395c98"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.12/tcfs-0.12.12-macos-x86_64.tar.gz"
      sha256 "5347d79f201ef0be85c58ef30134f02fb335c1493eb8c2e671f2e7474a2940b0"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.12/tcfs-0.12.12-linux-aarch64.tar.gz"
      sha256 "5cd7b649969f333042cc119532b846e3ba3c94499cad915474f8e2ee2af651d7"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.12/tcfs-0.12.12-linux-x86_64.tar.gz"
      sha256 "4a1d4c8f41ccd2b8f0430159277ef14a449de1b22740b7a608c4e2451a6d065b"
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
