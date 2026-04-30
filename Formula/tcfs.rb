# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.3, 7ddc1185fdcc589b1bfe35b1d9f906c69237962eb225dfd648ad09cf6cff0404, f397791c1cc1182a09bebee5c6926eea7cc11dc2cbafecfab8954721749d01b8,
#               7ed6a553aaa1db001bd5271b070c82d443acc17dbffb89ce8466a6d2a0d013fc, a8d2032e12476562c303ece1abcd06b4b8001bab601e475a727af61ec725414c

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.3"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.3/tcfs-0.12.3-macos-aarch64.tar.gz"
      sha256 "7ddc1185fdcc589b1bfe35b1d9f906c69237962eb225dfd648ad09cf6cff0404"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.3/tcfs-0.12.3-macos-x86_64.tar.gz"
      sha256 "f397791c1cc1182a09bebee5c6926eea7cc11dc2cbafecfab8954721749d01b8"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.3/tcfs-0.12.3-linux-aarch64.tar.gz"
      sha256 "a8d2032e12476562c303ece1abcd06b4b8001bab601e475a727af61ec725414c"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.3/tcfs-0.12.3-linux-x86_64.tar.gz"
      sha256 "7ed6a553aaa1db001bd5271b070c82d443acc17dbffb89ce8466a6d2a0d013fc"
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
