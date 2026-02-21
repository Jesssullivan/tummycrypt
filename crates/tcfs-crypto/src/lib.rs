//! tcfs-crypto: Client-side E2E encryption for TummyCrypt
//!
//! Architecture: Chunk-then-Encrypt with XChaCha20-Poly1305
//!
//! Pipeline: plaintext → FastCDC chunk → zstd compress → encrypt → BLAKE3 hash ciphertext → upload
//!
//! Key hierarchy:
//! ```text
//! Master Key (256-bit, Argon2id from passphrase)
//!   ├── File Encryption Key (per-file, 256-bit random, wrapped by master key)
//!   │   └── Chunk AEAD: XChaCha20-Poly1305 (key=file_key, nonce=random_192bit, AAD=chunk_idx||file_id)
//!   ├── Manifest Encryption Key (HKDF from master key, domain="tcfs-manifest")
//!   └── Name Encryption Key (HKDF from master key, domain="tcfs-names", AES-SIV)
//! ```

pub mod chunk;
pub mod kdf;
pub mod keys;
pub mod manifest;
pub mod names;
pub mod recovery;

pub use chunk::{decrypt_chunk, encrypt_chunk};
pub use kdf::{derive_master_key, MasterKey};
pub use keys::{
    derive_manifest_key, derive_name_key, generate_file_key, unwrap_key, wrap_key, FileKey,
};
pub use manifest::{EncryptedManifest, ManifestEntry};
pub use names::{decrypt_name, encrypt_name};
pub use recovery::{generate_mnemonic, mnemonic_to_master_key};

/// Size of a master key in bytes (256-bit)
pub const KEY_SIZE: usize = 32;

/// Size of an XChaCha20-Poly1305 nonce (192-bit)
pub const NONCE_SIZE: usize = 24;

/// Size of a Poly1305 authentication tag
pub const TAG_SIZE: usize = 16;
