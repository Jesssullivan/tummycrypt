# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.9, 3e620f30b64ea0ab5a2c65e3f5a03beaad02a732ffe613701041d2f3b23364b6, fb2dc3946378d45543fbd9483a4c70c10212d4bc18bca74ddb5b3c019494ab6f,
#               14d83125a1e79d9c51c16dc5aba8b15c99340a3c795682b36f38afc9ca42c092, 305cdc4c04cb306f912c34a0f7cd2c40a01be0c8269a746bed74b8a30f1f0d89

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.9"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.9/tcfs-0.12.9-macos-aarch64.tar.gz"
      sha256 "3e620f30b64ea0ab5a2c65e3f5a03beaad02a732ffe613701041d2f3b23364b6"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.9/tcfs-0.12.9-macos-x86_64.tar.gz"
      sha256 "fb2dc3946378d45543fbd9483a4c70c10212d4bc18bca74ddb5b3c019494ab6f"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.9/tcfs-0.12.9-linux-aarch64.tar.gz"
      sha256 "305cdc4c04cb306f912c34a0f7cd2c40a01be0c8269a746bed74b8a30f1f0d89"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.9/tcfs-0.12.9-linux-x86_64.tar.gz"
      sha256 "14d83125a1e79d9c51c16dc5aba8b15c99340a3c795682b36f38afc9ca42c092"
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
