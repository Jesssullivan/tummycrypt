# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.11, 2d184c016752e3ee62e525fdd7c398c22e63f9bbf3b354f54ee240981052a418, 0a963de41311dcf207824a605e9880733cec81fff61577efd2fb071ee34aa31f,
#               bc14dc7bd0d68a8700729e45c4a8abcbf2bdb87aa60f4f50025b6924b0072570, ce5b19232983a24ddbb6496de7bd3a15784bcb3440475854a057f410f3139731

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.11"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.11/tcfs-0.12.11-macos-aarch64.tar.gz"
      sha256 "2d184c016752e3ee62e525fdd7c398c22e63f9bbf3b354f54ee240981052a418"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.11/tcfs-0.12.11-macos-x86_64.tar.gz"
      sha256 "0a963de41311dcf207824a605e9880733cec81fff61577efd2fb071ee34aa31f"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.11/tcfs-0.12.11-linux-aarch64.tar.gz"
      sha256 "ce5b19232983a24ddbb6496de7bd3a15784bcb3440475854a057f410f3139731"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.11/tcfs-0.12.11-linux-x86_64.tar.gz"
      sha256 "bc14dc7bd0d68a8700729e45c4a8abcbf2bdb87aa60f4f50025b6924b0072570"
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
