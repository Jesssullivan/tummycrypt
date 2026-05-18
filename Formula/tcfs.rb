# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.13-rc1, c164a63a008dfe0d20c6f9320ecf61997a8c1ede880030a380acd9d9e3190df4, e0d82a125167b7b27543c5c14109d00981faee64829d29f237900018e7c2b0e3,
#               8f9c2a92390f5c7251a42e8f6603a99d176c0fd98f01fb20477ec5c32c94b9a5, 672c1bb85d799c9da0cdd9f1e8e93ef109f9bc7141f7e99fe187fcf0ac32bdf9

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.13-rc1"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc1/tcfs-0.12.13-rc1-macos-aarch64.tar.gz"
      sha256 "c164a63a008dfe0d20c6f9320ecf61997a8c1ede880030a380acd9d9e3190df4"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc1/tcfs-0.12.13-rc1-macos-x86_64.tar.gz"
      sha256 "e0d82a125167b7b27543c5c14109d00981faee64829d29f237900018e7c2b0e3"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc1/tcfs-0.12.13-rc1-linux-aarch64.tar.gz"
      sha256 "672c1bb85d799c9da0cdd9f1e8e93ef109f9bc7141f7e99fe187fcf0ac32bdf9"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc1/tcfs-0.12.13-rc1-linux-x86_64.tar.gz"
      sha256 "8f9c2a92390f5c7251a42e8f6603a99d176c0fd98f01fb20477ec5c32c94b9a5"
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
