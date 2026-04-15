# Homebrew formula for tcfs
# To use: brew tap tinyland-inc/tap && brew install tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.0, 4d5542641b6a1c7cbf99a70bf7d4f46ad0f21feb05999eeaff48c7ed6f31d779, 5864d0a2d8c80ee373e449133f5b6e47700ce7b73dc093b14a384ca708b88afc,
#               cd074311fe1d06aef1a0a5beef92f8d36fab15be32bb543348c0e7822c6c8f0a, 927b8c20855c8f28037a5cfcf6e4301063a833c67e5e9f9577fddfca3d7c89ae

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/tinyland-inc/tummycrypt"
  version "0.12.0"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-macos-aarch64.tar.gz"
      sha256 "4d5542641b6a1c7cbf99a70bf7d4f46ad0f21feb05999eeaff48c7ed6f31d779"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-macos-x86_64.tar.gz"
      sha256 "5864d0a2d8c80ee373e449133f5b6e47700ce7b73dc093b14a384ca708b88afc"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-linux-aarch64.tar.gz"
      sha256 "927b8c20855c8f28037a5cfcf6e4301063a833c67e5e9f9577fddfca3d7c89ae"
    else
      url "https://github.com/tinyland-inc/tummycrypt/releases/download/v0.12.0/tcfs-0.12.0-linux-x86_64.tar.gz"
      sha256 "cd074311fe1d06aef1a0a5beef92f8d36fab15be32bb543348c0e7822c6c8f0a"
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
