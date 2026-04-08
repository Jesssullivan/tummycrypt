# Homebrew formula for tcfs
# To use: brew tap tinyland-inc/tap && brew install tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.11.1, aa613ee414ea59661b35a950129bb7341aafcf310ee59ae5b0f7ae61d62bc8d0, 83ae22ae0d06c541b9cf520194070f0c65d0b9becb924c6c02f411c85e682d8a,
#               6bc85b0603c37686d2daddfd7835fd3b8960775710540d5f3841af7570043348, 504bf6b901240f9df5c54eef20ec4995a9bee8e50a0660971313bab64a8dbf5c

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/tinyland-inc/tummycrypt"
  version "0.11.1"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.1/tcfs-0.11.1-macos-aarch64.tar.gz"
      sha256 "aa613ee414ea59661b35a950129bb7341aafcf310ee59ae5b0f7ae61d62bc8d0"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.1/tcfs-0.11.1-macos-x86_64.tar.gz"
      sha256 "83ae22ae0d06c541b9cf520194070f0c65d0b9becb924c6c02f411c85e682d8a"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.1/tcfs-0.11.1-linux-aarch64.tar.gz"
      sha256 "504bf6b901240f9df5c54eef20ec4995a9bee8e50a0660971313bab64a8dbf5c"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.11.1/tcfs-0.11.1-linux-x86_64.tar.gz"
      sha256 "6bc85b0603c37686d2daddfd7835fd3b8960775710540d5f3841af7570043348"
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
