# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.5, 12904016e1b110db3c08fe542336680276f707121a88c2199e405bc21f43c64f, 95ace626def83a539d9d4ff9c91356d08c918576969cea55efefcb24261bb83a,
#               43012d893d11b94235134634aef1ebd601e789a1517c93dd53e4941d1e26c07b, 30ad45c0e4d3cd184de9dbeaa2bd269efa8df6dd6704db321b9beca4863dbf42

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.5"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.5/tcfs-0.12.5-macos-aarch64.tar.gz"
      sha256 "12904016e1b110db3c08fe542336680276f707121a88c2199e405bc21f43c64f"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.5/tcfs-0.12.5-macos-x86_64.tar.gz"
      sha256 "95ace626def83a539d9d4ff9c91356d08c918576969cea55efefcb24261bb83a"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.5/tcfs-0.12.5-linux-aarch64.tar.gz"
      sha256 "30ad45c0e4d3cd184de9dbeaa2bd269efa8df6dd6704db321b9beca4863dbf42"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.5/tcfs-0.12.5-linux-x86_64.tar.gz"
      sha256 "43012d893d11b94235134634aef1ebd601e789a1517c93dd53e4941d1e26c07b"
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
