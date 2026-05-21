# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.13-rc4, eced300f1e523daa8864970bdcee7d364b200070d68ded3de75f8da4ec585ec4, 132b60e50f6853d622de783ccfbf80eefbe54175e2e5ff446580c560f0547f56,
#               15c3cbdfd841a19c15a448142f2d766b2e6c222667f8b7c7f39a396ba671ef95, c5952a495a5d6f24918bcdf7fc8deb051dc5451d18db23b450fdfd979d1a52ca

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.13-rc4"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc4/tcfs-0.12.13-rc4-macos-aarch64.tar.gz"
      sha256 "eced300f1e523daa8864970bdcee7d364b200070d68ded3de75f8da4ec585ec4"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc4/tcfs-0.12.13-rc4-macos-x86_64.tar.gz"
      sha256 "132b60e50f6853d622de783ccfbf80eefbe54175e2e5ff446580c560f0547f56"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc4/tcfs-0.12.13-rc4-linux-aarch64.tar.gz"
      sha256 "c5952a495a5d6f24918bcdf7fc8deb051dc5451d18db23b450fdfd979d1a52ca"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc4/tcfs-0.12.13-rc4-linux-x86_64.tar.gz"
      sha256 "15c3cbdfd841a19c15a448142f2d766b2e6c222667f8b7c7f39a396ba671ef95"
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
