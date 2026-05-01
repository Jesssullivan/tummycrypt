# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.7, 5231cf490d860ca7642578a40e918e6ff9e8a5da42796aaf688573d5e03820f8, ac401a257f719056159a9ddaef8678c493409c1c1d7917efd67bf786d5dea996,
#               002ad638d3027677028eef65df8d741971135e340385ad88dd65ec09408d1e6b, c97c78eebdf7feaccaa511eba6927f2384c78b6d0b8840e51ea030732406619f

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.7"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.7/tcfs-0.12.7-macos-aarch64.tar.gz"
      sha256 "5231cf490d860ca7642578a40e918e6ff9e8a5da42796aaf688573d5e03820f8"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.7/tcfs-0.12.7-macos-x86_64.tar.gz"
      sha256 "ac401a257f719056159a9ddaef8678c493409c1c1d7917efd67bf786d5dea996"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.7/tcfs-0.12.7-linux-aarch64.tar.gz"
      sha256 "c97c78eebdf7feaccaa511eba6927f2384c78b6d0b8840e51ea030732406619f"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.7/tcfs-0.12.7-linux-x86_64.tar.gz"
      sha256 "002ad638d3027677028eef65df8d741971135e340385ad88dd65ec09408d1e6b"
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
