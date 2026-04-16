# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.2, 886153e63cfd4edc566c97d5378c7bf3df1ae3a03ea24b0362dfc78725afed08, c47a3ca1bcaf7985846ac5b1bdcd37b506ce7253fc1b376890aa5099775986aa,
#               aea3db922a30cb90b809b4019ecd980f2edc0c5a2537094bfd2235e035271395, 63904a04d0b034800393a979fb79479aee964746ed411e12f1d389dfaa4a6d02

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.2"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-macos-aarch64.tar.gz"
      sha256 "886153e63cfd4edc566c97d5378c7bf3df1ae3a03ea24b0362dfc78725afed08"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-macos-x86_64.tar.gz"
      sha256 "c47a3ca1bcaf7985846ac5b1bdcd37b506ce7253fc1b376890aa5099775986aa"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-linux-aarch64.tar.gz"
      sha256 "63904a04d0b034800393a979fb79479aee964746ed411e12f1d389dfaa4a6d02"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-linux-x86_64.tar.gz"
      sha256 "aea3db922a30cb90b809b4019ecd980f2edc0c5a2537094bfd2235e035271395"
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
