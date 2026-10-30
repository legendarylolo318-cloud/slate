use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{AeadCore, Aes256Gcm, KeyInit};
use std::fs;
use std::path::Path;
use zeroize::Zeroize;

use crate::SlateError;

const MAGIC: &[u8; 4] = b"SLAT";
const VERSION: u8 = 1;
/// 4 (magic) + 1 (version) + 12 (nonce) = 17 bytes header before ciphertext.
const HEADER_LEN: usize = 4 + 1 + 12;

pub struct Crypto {
    cipher: Aes256Gcm,
}

impl Crypto {
    /// Load or generate the master key and construct the cipher.
    ///
    /// Key sources (in order):
    /// 1. OS keyring (service "slate", entry "master-key")
    /// 2. File fallback at `config_dir/slate.key` (created with 0600 perms)
    pub fn new(config_dir: &Path) -> Result<Self, SlateError> {
        let mut raw_key = load_or_generate_key(config_dir)?;
        let key = aes_gcm::Key::<Aes256Gcm>::from_slice(&raw_key);
        let cipher = Aes256Gcm::new(key);
        raw_key.zeroize();
        Ok(Self { cipher })
    }

    /// A cipher with a fixed key, so tests never touch the keyring.
    #[cfg(test)]
    pub fn with_test_key() -> Self {
        let key = aes_gcm::Key::<Aes256Gcm>::from_slice(&[7u8; 32]);
        Self {
            cipher: Aes256Gcm::new(key),
        }
    }

    /// Encrypt plaintext. Returns `[magic][version][nonce][ciphertext+tag]`.
    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, SlateError> {
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let ciphertext = self
            .cipher
            .encrypt(&nonce, plaintext)
            .map_err(|e| SlateError::Crypto(e.to_string()))?;

        let mut out = Vec::with_capacity(HEADER_LEN + ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt data. If the magic header is absent, the data is assumed to be
    /// unencrypted plaintext (transparent migration) and returned as-is.
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>, SlateError> {
        if !is_encrypted(data) {
            return Ok(data.to_vec());
        }

        if data.len() < HEADER_LEN {
            return Err(SlateError::Crypto("encrypted data too short".into()));
        }

        let _version = data[4];
        let nonce = aes_gcm::Nonce::from_slice(&data[5..17]);
        let ciphertext = &data[HEADER_LEN..];

        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| SlateError::Crypto(e.to_string()))
    }
}

/// Check whether the data starts with the SLAT magic header.
pub fn is_encrypted(data: &[u8]) -> bool {
    data.len() >= 4 && &data[..4] == MAGIC
}

// ---------------------------------------------------------------------------
// Key management
// ---------------------------------------------------------------------------

fn load_or_generate_key(config_dir: &Path) -> Result<[u8; 32], SlateError> {
    // 1. Try OS keyring
    match load_key_from_keyring() {
        Ok(key) => return Ok(key),
        Err(e) => log::debug!("Keyring load failed (will try file): {e}"),
    }

    // 2. Try file fallback (only exists when the keyring was unavailable)
    let key_path = config_dir.join("slate.key");
    if let Ok(key) = load_key_from_file(&key_path) {
        // Migrate into the keyring and drop the plaintext copy, but only once
        // the keyring demonstrably returns the same key.
        match store_key_in_keyring(&key).and_then(|()| load_key_from_keyring()) {
            Ok(stored) if stored == key => {
                if fs::remove_file(&key_path).is_ok() {
                    log::info!("Master key migrated into the OS keyring; key file removed");
                }
            }
            Ok(_) => log::debug!("Keyring returned a different key; keeping key file"),
            Err(e) => log::debug!("Could not store key in keyring: {e}"),
        }
        return Ok(key);
    }

    // 3. Generate new key
    log::info!("Generating new encryption master key");
    let mut key = [0u8; 32];
    aes_gcm::aead::OsRng.fill_bytes(&mut key);

    // The key never sits next to the data it protects unless the keyring is
    // unusable; in that case a 0600 file is the only way to survive a restart.
    match store_key_in_keyring(&key).and_then(|()| load_key_from_keyring()) {
        Ok(stored) if stored == key => log::info!("Master key stored in OS keyring"),
        other => {
            if let Err(e) = other {
                log::info!("Keyring unavailable ({e}), using file-based key");
            }
            store_key_in_file(&key_path, &key)?;
        }
    }

    Ok(key)
}

fn load_key_from_keyring() -> Result<[u8; 32], String> {
    let entry = keyring::Entry::new("slate", "master-key").map_err(|e| e.to_string())?;
    let secret = entry.get_secret().map_err(|e| e.to_string())?;
    if secret.len() != 32 {
        return Err(format!("keyring key wrong length: {}", secret.len()));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&secret);
    Ok(key)
}

fn store_key_in_keyring(key: &[u8; 32]) -> Result<(), String> {
    let entry = keyring::Entry::new("slate", "master-key").map_err(|e| e.to_string())?;
    entry.set_secret(key).map_err(|e| e.to_string())
}

fn load_key_from_file(path: &Path) -> Result<[u8; 32], SlateError> {
    let data = fs::read(path)?;
    if data.len() != 32 {
        return Err(SlateError::Crypto(format!(
            "key file wrong length: {}",
            data.len()
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&data);
    Ok(key)
}

fn store_key_in_file(path: &Path, key: &[u8; 32]) -> Result<(), SlateError> {
    use std::io::Write;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    // Created 0600 from the start, so there is no window with looser bits.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(key)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }

    let path_display = path.display();
    log::info!("Master key stored at {path_display} (mode 0600)");
    Ok(())
}

// Use rand's fill_bytes via the aead OsRng re-export
use aes_gcm::aead::rand_core::RngCore;

/// The master key must survive a restart, so the keyring backend must be the
/// OS store, not keyring's in-memory mock (what a build without the platform
/// feature silently gets). The mock keeps nothing between `Entry` values.
#[cfg(all(test, windows))]
mod keyring_backend_tests {
    #[test]
    fn keyring_is_the_persistent_os_store() {
        let user = format!("selftest-{}", uuid::Uuid::new_v4().simple());
        let write = keyring::Entry::new("slate-test", &user).unwrap();
        write.set_password("persisted").unwrap();
        let read = keyring::Entry::new("slate-test", &user).unwrap();
        let got = read.get_password();
        let _ = write.delete_credential();
        assert_eq!(
            got.unwrap(),
            "persisted",
            "keyring is not the Windows Credential Manager"
        );
    }
}
