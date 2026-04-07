# Homebrew formula for tcfs
# To use: brew tap tinyland-inc/tap && brew install tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.11.0, d144b3cb631b156b3783c4934b84c32b862c026562d003aab187d2da64f82a97, e82c7a231ebd5eaa19a2bb53daa5e5f0f42e023fe7938ef53f6ab7b23c78b6c0,
#               7816145c83c6bef054716a822dec1199058910fa5abe1d35947e5da5cffd99ee, 2f1e4174a8c8eeefd65acb2b04856ef7974c2784cdad8b423a256da2d94d9cf5

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/tinyland-inc/tummycrypt"
  version "0.11.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.0/tcfs-0.11.0-macos-aarch64.tar.gz"
      sha256 "d144b3cb631b156b3783c4934b84c32b862c026562d003aab187d2da64f82a97"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.0/tcfs-0.11.0-macos-x86_64.tar.gz"
      sha256 "e82c7a231ebd5eaa19a2bb53daa5e5f0f42e023fe7938ef53f6ab7b23c78b6c0"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.0/tcfs-0.11.0-linux-aarch64.tar.gz"
      sha256 "2f1e4174a8c8eeefd65acb2b04856ef7974c2784cdad8b423a256da2d94d9cf5"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.0/tcfs-0.11.0-linux-x86_64.tar.gz"
      sha256 "7816145c83c6bef054716a822dec1199058910fa5abe1d35947e5da5cffd99ee"
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
