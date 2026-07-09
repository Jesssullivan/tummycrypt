# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.17, 57062c9e5345d86c1576cf77a05ac44af44da7943e0b58e9d8db92c46d47d35a, 42344fbf1b957ea6d7daab048013042dda6b105ef4508fd3d3a0fce683330af8,
#               56d93081f4959cf40211a16ff5b24ccbcc5d0cbaf8539e5a6bfddff34084a609, 0a1f8c9fcd913fc6982f5ff660a0bfe8a6fb9d188ae977592152c2715666866b

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.17"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.17/tcfs-0.12.17-macos-aarch64.tar.gz"
      sha256 "57062c9e5345d86c1576cf77a05ac44af44da7943e0b58e9d8db92c46d47d35a"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.17/tcfs-0.12.17-macos-x86_64.tar.gz"
      sha256 "42344fbf1b957ea6d7daab048013042dda6b105ef4508fd3d3a0fce683330af8"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.17/tcfs-0.12.17-linux-aarch64.tar.gz"
      sha256 "0a1f8c9fcd913fc6982f5ff660a0bfe8a6fb9d188ae977592152c2715666866b"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.17/tcfs-0.12.17-linux-x86_64.tar.gz"
      sha256 "56d93081f4959cf40211a16ff5b24ccbcc5d0cbaf8539e5a6bfddff34084a609"
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
