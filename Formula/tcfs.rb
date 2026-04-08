# Homebrew formula for tcfs
# To use: brew tap tinyland-inc/tap && brew install tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.0, 40317da373af9c461bb11edd40e8bd1c7da94fdca880c6c3fefb14f8246a0ce1, 6dcae1de29ea0c4f54399cddfa0d67bbf37cae81f31ab2fdadbd793675eee0e9,
#               04229e09c56cd7f51496f3a81c4dc186d902abc515f39d8fe1052c9d52d54a4b, 23c06ea72f0d6772457eb53f0dc7da739153ebcd3f457b87e422b365d361279a

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/tinyland-inc/tummycrypt"
  version "0.12.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-macos-aarch64.tar.gz"
      sha256 "40317da373af9c461bb11edd40e8bd1c7da94fdca880c6c3fefb14f8246a0ce1"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-macos-x86_64.tar.gz"
      sha256 "6dcae1de29ea0c4f54399cddfa0d67bbf37cae81f31ab2fdadbd793675eee0e9"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-linux-aarch64.tar.gz"
      sha256 "23c06ea72f0d6772457eb53f0dc7da739153ebcd3f457b87e422b365d361279a"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-linux-x86_64.tar.gz"
      sha256 "04229e09c56cd7f51496f3a81c4dc186d902abc515f39d8fe1052c9d52d54a4b"
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
