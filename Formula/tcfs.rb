# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.2, f1985357e67b47d42c66a0ba12a002f0017ac9f169640d6662b5870f12711000, 24fadd9a7e45c2cc30ea07daea02d933d98727cfc20c3f4fc408d5e61df24a3c,
#               1efc16245f5dbbe5e3657d8c9f253f02b185343528ac919317934a9cc789afb0, b825491d76149c4988600b70ade0772f4f5b64f59d9d6fdbe694164dda380710

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.2"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-macos-aarch64.tar.gz"
      sha256 "f1985357e67b47d42c66a0ba12a002f0017ac9f169640d6662b5870f12711000"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-macos-x86_64.tar.gz"
      sha256 "24fadd9a7e45c2cc30ea07daea02d933d98727cfc20c3f4fc408d5e61df24a3c"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-linux-aarch64.tar.gz"
      sha256 "b825491d76149c4988600b70ade0772f4f5b64f59d9d6fdbe694164dda380710"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.2/tcfs-0.12.2-linux-x86_64.tar.gz"
      sha256 "1efc16245f5dbbe5e3657d8c9f253f02b185343528ac919317934a9cc789afb0"
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
