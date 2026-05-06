# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.8, a7209d59f9c7073b0bfa9a46d79737f29e73022b8263c42ac5b6506c7ea054e0, 62792fbe51315f1d3facee1123be9080805345da7823a875b527d09c41dc1c55,
#               50eea6c649d6b07ea98233beb231b03939ef2494524bdccc49ee7c3f3f85b755, 52ba64b2aa4166273ec0c29aa10a716a0835e0a218d596da6125bad90e2e331f

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.8"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.8/tcfs-0.12.8-macos-aarch64.tar.gz"
      sha256 "a7209d59f9c7073b0bfa9a46d79737f29e73022b8263c42ac5b6506c7ea054e0"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.8/tcfs-0.12.8-macos-x86_64.tar.gz"
      sha256 "62792fbe51315f1d3facee1123be9080805345da7823a875b527d09c41dc1c55"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.8/tcfs-0.12.8-linux-aarch64.tar.gz"
      sha256 "52ba64b2aa4166273ec0c29aa10a716a0835e0a218d596da6125bad90e2e331f"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.8/tcfs-0.12.8-linux-x86_64.tar.gz"
      sha256 "50eea6c649d6b07ea98233beb231b03939ef2494524bdccc49ee7c3f3f85b755"
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
