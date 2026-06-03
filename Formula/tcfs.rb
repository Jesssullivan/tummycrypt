# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.14, 55719e35b624df25386baf63d74247c39a25e857b7c3855cfd4edd6cfae69175, f171ee9c26a843dd29aa99a17ea73536da635b8c96aba4ba3bfbc183608ad7f2,
#               56c158ec2ce4e598a73d1b86fb2b901535ad03468ceb2436d5b9c3122a0d036c, 48618f077f52792a0d418ea97400c9e41a51ae848a29eb67fcc8e24267056059

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.14"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.14/tcfs-0.12.14-macos-aarch64.tar.gz"
      sha256 "55719e35b624df25386baf63d74247c39a25e857b7c3855cfd4edd6cfae69175"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.14/tcfs-0.12.14-macos-x86_64.tar.gz"
      sha256 "f171ee9c26a843dd29aa99a17ea73536da635b8c96aba4ba3bfbc183608ad7f2"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.14/tcfs-0.12.14-linux-aarch64.tar.gz"
      sha256 "48618f077f52792a0d418ea97400c9e41a51ae848a29eb67fcc8e24267056059"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.14/tcfs-0.12.14-linux-x86_64.tar.gz"
      sha256 "56c158ec2ce4e598a73d1b86fb2b901535ad03468ceb2436d5b9c3122a0d036c"
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
