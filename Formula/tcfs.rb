# Homebrew formula for tcfs
# To use:
#   brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
#   git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
#   brew install Jesssullivan/tummycrypt/tcfs
#
# This template is used by CI to generate the versioned formula.
# Placeholders: 0.12.13-rc2, ec669f9bbaea732ed44ce4ceab6d2d9d4c5569d38e89ec12efab80cdcf836510, 4e4595cf36d200cacbd6c7aa5c4f9ecfc0501ffa4cb168b5d02ca6eca95641ec,
#               94281958dfa9dd6e4c3c46528ab2c562675c4964097e79235358e8343078ee3c, e44e1301cf1f308b8a38f214102dabe618b417eb17cc2ec3f38f245e0af8495e

class Tcfs < Formula
  desc "FOSS self-hosted odrive replacement — FUSE-based, SeaweedFS-backed file sync"
  homepage "https://github.com/Jesssullivan/tummycrypt"
  version "0.12.13-rc2"
  license any_of: ["MIT", "Apache-2.0"]

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc2/tcfs-0.12.13-rc2-macos-aarch64.tar.gz"
      sha256 "ec669f9bbaea732ed44ce4ceab6d2d9d4c5569d38e89ec12efab80cdcf836510"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc2/tcfs-0.12.13-rc2-macos-x86_64.tar.gz"
      sha256 "4e4595cf36d200cacbd6c7aa5c4f9ecfc0501ffa4cb168b5d02ca6eca95641ec"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc2/tcfs-0.12.13-rc2-linux-aarch64.tar.gz"
      sha256 "e44e1301cf1f308b8a38f214102dabe618b417eb17cc2ec3f38f245e0af8495e"
    else
      url "https://github.com/Jesssullivan/tummycrypt/releases/download/v0.12.13-rc2/tcfs-0.12.13-rc2-linux-x86_64.tar.gz"
      sha256 "94281958dfa9dd6e4c3c46528ab2c562675c4964097e79235358e8343078ee3c"
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
