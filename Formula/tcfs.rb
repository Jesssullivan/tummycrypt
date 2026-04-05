# Homebrew formula for tcfs
# To use: brew tap tinyland-inc/tap && brew install tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.10.0, 00fcbab82c5863dc739e228bf73690dbb51b991e0ac7ec5fd5e86ddaee1fd1f1, 44cd57590ca5000a8c3e93540659a7beb8ea1110a89bb80f1bf63407e496aab0,
#               bb6cb2b1b42d797f28c3ac79dc6d4c753a6ee499a82d903f11e3378d4324aae7, d6547f6e2cccb58f8591632ac3b516a4b1ac348902bc129f39eaf49621f89deb

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/tinyland-inc/tummycrypt"
  version "0.10.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.10.0/tcfs-0.10.0-macos-aarch64.tar.gz"
      sha256 "00fcbab82c5863dc739e228bf73690dbb51b991e0ac7ec5fd5e86ddaee1fd1f1"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.10.0/tcfs-0.10.0-macos-x86_64.tar.gz"
      sha256 "44cd57590ca5000a8c3e93540659a7beb8ea1110a89bb80f1bf63407e496aab0"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.10.0/tcfs-0.10.0-linux-aarch64.tar.gz"
      sha256 "d6547f6e2cccb58f8591632ac3b516a4b1ac348902bc129f39eaf49621f89deb"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.10.0/tcfs-0.10.0-linux-x86_64.tar.gz"
      sha256 "bb6cb2b1b42d797f28c3ac79dc6d4c753a6ee499a82d903f11e3378d4324aae7"
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
