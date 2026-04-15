# tummycrypt: Repo-Specific Review Rules

Inherits all rules from `_org-enforced-rules.md`.

## Rust

- Every `unsafe` block requires a `// SAFETY:` comment explaining why it is sound and what invariants must hold.
- No `unwrap()` or `expect()` in library code. Use `?` operator and proper `Result`/`Option` handling.
- Error types must implement `std::fmt::Display` and `std::error::Error`.
- Use `#[must_use]` on functions that return values the caller should not ignore.
- Prefer `&str` over `String` in function parameters where ownership is not needed.
- Run `cargo clippy` clean before merging.

## Cryptography

- Only use audited cryptography crates (`ring`, `rustcrypto` ecosystem, `sodiumoxide`). Never hand-roll crypto.
- Key material must be zeroized on drop (`zeroize` crate).
- Review any changes to encryption/decryption paths with heightened scrutiny.

## Swift / Apple Integration

- FileProvider extension must handle all NSFileProviderError cases.
- Rust-Swift FFI: validate all raw pointers at the boundary. Null checks are mandatory.
- Use `@objc` only where required for system framework callbacks.
- Memory passed across FFI must have clear ownership semantics documented in comments.

## Nix

- Build expressions must produce reproducible outputs.
- Flake checks should include `cargo test` and `cargo clippy`.
