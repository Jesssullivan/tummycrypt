# Homebrew formula for tcfs
# To use:
#   brew tap Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt --branch homebrew-tap
#   brew install tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.1, f2524758f152122e2a8efe66a02e4de904b52d5b51e24f1909b3b03899785d6a, a7b77bdde41bce65e674f2619c2fcb49948ad7c90c1fc737d86af4226c5baf84,
#               76f04bd02228b3efff3eca48248c81e72385aa33c9e48409ada16f74d2bd307d, 6d105a2ffa70f3441bfc5f5325f1961d1ce1f5ffeef69fd6f44ff00782a21d09

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.1"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.1/tcfs-0.12.1-macos-aarch64.tar.gz"
      sha256 "f2524758f152122e2a8efe66a02e4de904b52d5b51e24f1909b3b03899785d6a"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.1/tcfs-0.12.1-macos-x86_64.tar.gz"
      sha256 "a7b77bdde41bce65e674f2619c2fcb49948ad7c90c1fc737d86af4226c5baf84"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.1/tcfs-0.12.1-linux-aarch64.tar.gz"
      sha256 "6d105a2ffa70f3441bfc5f5325f1961d1ce1f5ffeef69fd6f44ff00782a21d09"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.1/tcfs-0.12.1-linux-x86_64.tar.gz"
      sha256 "76f04bd02228b3efff3eca48248c81e72385aa33c9e48409ada16f74d2bd307d"
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
