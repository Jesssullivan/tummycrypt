# Homebrew formula for tcfs
# To use: brew tap tinyland-inc/tap && brew install tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: __VERSION__, __SHA256_DARWIN_ARM64__, __SHA256_DARWIN_X86_64__,
#               __SHA256_LINUX_X86_64__, __SHA256_LINUX_ARM64__

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/tinyland-inc/tummycrypt"
  version "__VERSION__"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v__VERSION__/tcfs-__VERSION__-macos-aarch64.tar.gz"
      sha256 "__SHA256_DARWIN_ARM64__"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v__VERSION__/tcfs-__VERSION__-macos-x86_64.tar.gz"
      sha256 "__SHA256_DARWIN_X86_64__"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v__VERSION__/tcfs-__VERSION__-linux-aarch64.tar.gz"
      sha256 "__SHA256_LINUX_ARM64__"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v__VERSION__/tcfs-__VERSION__-linux-x86_64.tar.gz"
      sha256 "__SHA256_LINUX_X86_64__"
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
