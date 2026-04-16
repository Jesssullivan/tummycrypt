# Canonical tcfs homebrew-tap

Homebrew formulae for [TummyCrypt/tcfs](https://github.com/Jesssullivan/tummycrypt).

## Usage

```bash
brew tap --custom-remote Jesssullivan/tummycrypt https://github.com/Jesssullivan/tummycrypt.git
git -C "$(brew --repo Jesssullivan/tummycrypt)" fetch origin homebrew-tap
git -C "$(brew --repo Jesssullivan/tummycrypt)" checkout homebrew-tap
brew install Jesssullivan/tummycrypt/tcfs
```
