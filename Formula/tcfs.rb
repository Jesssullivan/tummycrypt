# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.13-rc3, b6b2e3bee55bf489f87b485b00323ad2120a142d47e4ff70c1610ec1a38c1997, 1b106b0946c12d4fe635ce57f6f0fbb84069b724e018e8138af6dfd266131814,
#               1dd17c3ac119b8495aed45402d8c9f566c1430573831d0f98172f1cfc9ce77c5, 44acbd631c9e4925cae820f2f2ae4b7a7004289218952a13e773c076f469dad9

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.13-rc3"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc3/tcfs-0.12.13-rc3-macos-aarch64.tar.gz"
      sha256 "b6b2e3bee55bf489f87b485b00323ad2120a142d47e4ff70c1610ec1a38c1997"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc3/tcfs-0.12.13-rc3-macos-x86_64.tar.gz"
      sha256 "1b106b0946c12d4fe635ce57f6f0fbb84069b724e018e8138af6dfd266131814"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc3/tcfs-0.12.13-rc3-linux-aarch64.tar.gz"
      sha256 "44acbd631c9e4925cae820f2f2ae4b7a7004289218952a13e773c076f469dad9"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc3/tcfs-0.12.13-rc3-linux-x86_64.tar.gz"
      sha256 "1dd17c3ac119b8495aed45402d8c9f566c1430573831d0f98172f1cfc9ce77c5"
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
