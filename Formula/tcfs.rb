# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.10, 23cb1fb6460d0a02f988ea731efd936a577986a70438d5ac13669c6407b802fb, 52f5fc09aee9fbbed616a311b1b576b13f5b58da99e26f8f21ed2f8400df606a,
#               140c0ae7c0dacea33cd9246f89348cf21e62501098d4af235490628bd76333dd, 78d0a428918819fe0a54e37c455117003c9426f2e50c0d9160f9508d10618b70

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.10"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.10/tcfs-0.12.10-macos-aarch64.tar.gz"
      sha256 "23cb1fb6460d0a02f988ea731efd936a577986a70438d5ac13669c6407b802fb"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.10/tcfs-0.12.10-macos-x86_64.tar.gz"
      sha256 "52f5fc09aee9fbbed616a311b1b576b13f5b58da99e26f8f21ed2f8400df606a"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.10/tcfs-0.12.10-linux-aarch64.tar.gz"
      sha256 "78d0a428918819fe0a54e37c455117003c9426f2e50c0d9160f9508d10618b70"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.10/tcfs-0.12.10-linux-x86_64.tar.gz"
      sha256 "140c0ae7c0dacea33cd9246f89348cf21e62501098d4af235490628bd76333dd"
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
