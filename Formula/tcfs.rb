# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.6, e9df7fbe756b68fa643dc53b566fd8f35025a340f5b932f35835581e77ac6e35, 99943220a60188e6f4f445ce9f8c75ea4c560694ab15a49976902330999f4aa7,
#               25432bbbd7cc13145c0fa5786e2ed791e76ab62a7549d42df88de3406464361e, 7ecb54cf1f005f5ca973b8c31827de45ef2379e863d9e34dcb1b997e607b1c3a

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.6"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.6/tcfs-0.12.6-macos-aarch64.tar.gz"
      sha256 "e9df7fbe756b68fa643dc53b566fd8f35025a340f5b932f35835581e77ac6e35"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.6/tcfs-0.12.6-macos-x86_64.tar.gz"
      sha256 "99943220a60188e6f4f445ce9f8c75ea4c560694ab15a49976902330999f4aa7"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.6/tcfs-0.12.6-linux-aarch64.tar.gz"
      sha256 "7ecb54cf1f005f5ca973b8c31827de45ef2379e863d9e34dcb1b997e607b1c3a"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.6/tcfs-0.12.6-linux-x86_64.tar.gz"
      sha256 "25432bbbd7cc13145c0fa5786e2ed791e76ab62a7549d42df88de3406464361e"
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
