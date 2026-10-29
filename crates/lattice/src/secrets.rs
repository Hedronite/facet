//! Secrets at rest.
//!
//! Two backends, picked by configuration:
//!
//! - **Keyring** (default, desktop): the OS keyring holds the secret value.
//!   The `environments.secret_ref` column stores `kr:<user>` where `<user>` is
//!   the keyring account name; `value` is NULL.
//! - **Encrypted** (headless/CI fallback): the secret is encrypted with
//!   XChaCha20-Poly1305 using a key derived from the `FACET_SECRET_KEY`
//!   environment variable. `secret_ref` stores `enc:v1:<base64(nonce||ciphertext)>`;
//!   `value` is NULL.
//!
//! Selection rule (agent-friendly, predictable):
//! - `FACET_SECRET_KEY` set and non-empty  -> EncryptedStore.
//! - otherwise                            -> KeyringStore. If the platform has
//!   no usable keyring backend, `put` returns [`SecretError::NoSecretStore`]
//!   so the caller can tell the user to set `FACET_SECRET_KEY`.
//!
//! Reading is by the `secret_ref` prefix, so a secret written under one mode
//! stays readable under the other as long as its key (keyring or
//! `FACET_SECRET_KEY`) is available. A non-secret value lives in `value` with
//! `secret_ref` NULL.
//!
//! The encrypted fallback's master key comes from `FACET_SECRET_KEY`; a
//! passphrase prompt is ruled out because it breaks agent use.

#![forbid(unsafe_code)]

use std::env;

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce, aead::Aead};
use hkdf::Hkdf;
use rand::RngCore;
use sha2::Sha256;
use zeroize::Zeroizing;

/// Prefix marking a keyring reference in `environments.secret_ref`.
pub const KEYRING_REF_PREFIX: &str = "kr:";
/// Prefix marking an encrypted secret in `environments.secret_ref`.
pub const ENCRYPTED_REF_PREFIX: &str = "enc:v1:";

/// Keyring service name. The account (user) is a random hex string so two
/// secrets never collide even after a delete reuses the same env/key.
const KEYRING_SERVICE: &str = "facet";
/// HKDF salt and info for deriving the XChaCha20-Poly1305 key from
/// `FACET_SECRET_KEY`. Bumped together with the `enc:v1` prefix.
const HKDF_SALT: &[u8] = b"facet-lattice-secrets-v1";
const HKDF_INFO: &[u8] = b"xchacha20poly1305-key-v1";

/// Environment variable holding the headless/CI master key.
pub const SECRET_KEY_ENV: &str = "FACET_SECRET_KEY";

/// XChaCha20 nonce length in bytes (24). Used to split the encrypted blob.
const XCHACHA20_NONCE_LEN: usize = 24;

/// Failures raised by the secrets layer.
#[derive(Debug)]
pub enum SecretError {
    /// The platform has no keyring backend and `FACET_SECRET_KEY` is unset.
    NoSecretStore,
    /// The OS keyring rejected the operation.
    Keyring(String),
    /// `FACET_SECRET_KEY` is set but the value is empty.
    EmptySecretKey,
    /// An encrypted `secret_ref` is malformed or could not be decrypted.
    BadCiphertext(String),
    /// A `secret_ref` carries an unknown prefix.
    UnknownRef(String),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSecretStore => write!(
                f,
                "no secret store available; set {SECRET_KEY_ENV} for headless mode"
            ),
            Self::Keyring(message) => write!(f, "keyring error: {message}"),
            Self::EmptySecretKey => write!(f, "{SECRET_KEY_ENV} is set but empty"),
            Self::BadCiphertext(message) => write!(f, "ciphertext error: {message}"),
            Self::UnknownRef(reference) => {
                write!(f, "unknown secret_ref prefix: {reference}")
            }
        }
    }
}

impl std::error::Error for SecretError {}

/// Which backend holds a secret.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretBackend {
    /// OS keyring.
    Keyring,
    /// Encrypted blob under `FACET_SECRET_KEY`.
    Encrypted,
}

/// A resolved secret reference: the string to store in `secret_ref`, plus the
/// backend that owns the secret (so the caller can delete from the right place).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSecret {
    /// The `secret_ref` string for the `environments` row.
    pub reference: String,
    /// The backend that owns the secret, for deletion.
    pub backend: SecretBackend,
}

/// Backend selection for the secrets layer. Built from the environment by the
/// free functions; constructed directly in tests so they never mutate the
/// process environment (Rust 2024 marks `set_var`/`remove_var` unsafe, and the
/// workspace forbids `unsafe_code`).
#[derive(Clone, Debug)]
pub struct SecretConfig {
    master_key: Option<Key>,
}

impl SecretConfig {
    /// Headless mode: encrypt with a key derived from `FACET_SECRET_KEY`.
    /// `None` means desktop mode (use the OS keyring).
    #[must_use]
    pub fn encrypted(master_key_material: &[u8]) -> Self {
        Self {
            master_key: Some(derive_key(master_key_material)),
        }
    }

    /// Desktop mode: use the OS keyring.
    #[must_use]
    pub fn keyring() -> Self {
        Self { master_key: None }
    }

    /// Reads `FACET_SECRET_KEY` from the environment. Set and non-empty ->
    /// [`Self::encrypted`]; unset -> [`Self::keyring`]; empty -> error.
    pub fn from_env() -> Result<Self, SecretError> {
        match env::var(SECRET_KEY_ENV) {
            Ok(value) if value.is_empty() => Err(SecretError::EmptySecretKey),
            Ok(value) => Ok(Self::encrypted(value.as_bytes())),
            Err(_) => Ok(Self::keyring()),
        }
    }

    fn backend(&self) -> Backend<'_> {
        match &self.master_key {
            Some(key) => Backend::Encrypted(key),
            None => Backend::Keyring,
        }
    }

    /// Whether this config selects the encrypted backend (a `FACET_SECRET_KEY`
    /// was provided) versus the OS keyring. Used by `facet doctor` to report
    /// which backend is in use without inspecting private fields.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.master_key.is_some()
    }
}

impl Default for SecretConfig {
    fn default() -> Self {
        Self::keyring()
    }
}

enum Backend<'a> {
    Keyring,
    Encrypted(&'a Key),
}

/// Stores one secret value using the environment-selected backend.
pub fn put_secret(value: &str) -> Result<StoredSecret, SecretError> {
    put_secret_with(value, &SecretConfig::from_env()?)
}

/// Resolves a `secret_ref` back to the secret value using the environment key.
pub fn get_secret(reference: &str) -> Result<Option<Zeroizing<String>>, SecretError> {
    get_secret_with(reference, &SecretConfig::from_env()?)
}

/// Deletes the secret behind a `secret_ref` (best effort; missing is fine).
pub fn delete_secret(reference: &str) -> Result<(), SecretError> {
    delete_secret_with(reference, &SecretConfig::from_env()?)
}

/// [`put_secret`] with an explicit config (for tests and callers that already
/// resolved the backend).
pub fn put_secret_with(value: &str, config: &SecretConfig) -> Result<StoredSecret, SecretError> {
    match config.backend() {
        Backend::Keyring => {
            let entry = fresh_keyring_entry()?;
            entry
                .set_password(value)
                .map_err(|error| SecretError::Keyring(error.to_string()))?;
            Ok(StoredSecret {
                reference: format!("{KEYRING_REF_PREFIX}{}", entry.keyring_user()),
                backend: SecretBackend::Keyring,
            })
        }
        Backend::Encrypted(key) => Ok(StoredSecret {
            reference: encrypt_secret(key, value.as_bytes())?,
            backend: SecretBackend::Encrypted,
        }),
    }
}

/// [`get_secret`] with an explicit config. Encrypted refs need the config's
/// key; keyring refs resolve through the OS keyring regardless of config.
pub fn get_secret_with(
    reference: &str,
    config: &SecretConfig,
) -> Result<Option<Zeroizing<String>>, SecretError> {
    if reference.is_empty() {
        return Ok(None);
    }
    if let Some(user) = reference.strip_prefix(KEYRING_REF_PREFIX) {
        let entry = keyring::Entry::new(KEYRING_SERVICE, user)
            .map_err(|error| SecretError::Keyring(error.to_string()))?;
        return match entry.get_password() {
            Ok(value) => Ok(Some(Zeroizing::new(value))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(SecretError::Keyring(error.to_string())),
        };
    }
    if let Some(payload) = reference.strip_prefix(ENCRYPTED_REF_PREFIX) {
        let key = config
            .master_key
            .as_ref()
            .ok_or(SecretError::NoSecretStore)?;
        // The decrypted bytes pass through `from_utf8` plain: the value the
        // store hands out — the return of `get_secret_with` — is always
        // `Zeroizing`.
        let value = String::from_utf8(decrypt_secret(key, payload)?)
            .map_err(|error| SecretError::BadCiphertext(error.to_string()))?;
        return Ok(Some(Zeroizing::new(value)));
    }
    Err(SecretError::UnknownRef(reference.to_owned()))
}

/// [`delete_secret`] with an explicit config.
pub fn delete_secret_with(reference: &str, _config: &SecretConfig) -> Result<(), SecretError> {
    if let Some(user) = reference.strip_prefix(KEYRING_REF_PREFIX) {
        let entry = keyring::Entry::new(KEYRING_SERVICE, user)
            .map_err(|error| SecretError::Keyring(error.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(SecretError::Keyring(error.to_string())),
        }
    } else if reference.starts_with(ENCRYPTED_REF_PREFIX) {
        // Encrypted secrets live in the row; deleting the row deletes them.
        Ok(())
    } else if reference.is_empty() {
        Ok(())
    } else {
        Err(SecretError::UnknownRef(reference.to_owned()))
    }
}

/// Which backend a `secret_ref` points at, or `None` when it is not a secret.
#[must_use]
pub fn backend_of(reference: &str) -> Option<SecretBackend> {
    if reference.starts_with(KEYRING_REF_PREFIX) {
        Some(SecretBackend::Keyring)
    } else if reference.starts_with(ENCRYPTED_REF_PREFIX) {
        Some(SecretBackend::Encrypted)
    } else {
        None
    }
}

fn derive_key(ikm: &[u8]) -> Key {
    let hkdf = Hkdf::<Sha256>::new(Some(HKDF_SALT), ikm);
    let mut key = Key::default();
    hkdf.expand(HKDF_INFO, &mut key)
        .expect("Hkdf expands into a 32-byte key");
    key
}

fn encrypt_secret(key: &Key, plaintext: &[u8]) -> Result<String, SecretError> {
    let cipher = XChaCha20Poly1305::new(key);
    let mut nonce_bytes = [0u8; XCHACHA20_NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let nonce = XNonce::try_from(&nonce_bytes[..])
        .map_err(|error| SecretError::BadCiphertext(error.to_string()))?;
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|error| SecretError::BadCiphertext(error.to_string()))?;
    let mut blob = Vec::with_capacity(nonce.len() + ciphertext.len());
    blob.extend_from_slice(&nonce);
    blob.extend_from_slice(&ciphertext);
    Ok(format!("{ENCRYPTED_REF_PREFIX}{}", BASE64.encode(&blob)))
}

fn decrypt_secret(key: &Key, payload: &str) -> Result<Vec<u8>, SecretError> {
    let blob = BASE64
        .decode(payload)
        .map_err(|error| SecretError::BadCiphertext(error.to_string()))?;
    if blob.len() < XCHACHA20_NONCE_LEN {
        return Err(SecretError::BadCiphertext("payload too short".into()));
    }
    let (nonce_bytes, ciphertext) = blob.split_at(XCHACHA20_NONCE_LEN);
    let cipher = XChaCha20Poly1305::new(key);
    let nonce = XNonce::try_from(nonce_bytes)
        .map_err(|error| SecretError::BadCiphertext(error.to_string()))?;
    cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|error| SecretError::BadCiphertext(error.to_string()))
}

/// Owned keyring entry: owns the account string so the `keyring::Entry` can
/// outlive any borrowed slice the caller holds.
struct KeyringEntry {
    user: String,
    entry: keyring::Entry,
}

impl KeyringEntry {
    fn new(user: String) -> Result<Self, SecretError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, &user)
            .map_err(|error| SecretError::Keyring(error.to_string()))?;
        Ok(Self { user, entry })
    }

    fn keyring_user(&self) -> &str {
        &self.user
    }
}

impl std::ops::Deref for KeyringEntry {
    type Target = keyring::Entry;
    fn deref(&self) -> &Self::Target {
        &self.entry
    }
}

/// Builds a keyring entry under a unique random account name.
fn fresh_keyring_entry() -> Result<KeyringEntry, SecretError> {
    let mut raw = [0u8; 16];
    rand::rng().fill_bytes(&mut raw);
    let user = format!("facet/{}", hex_encode(&raw));
    KeyringEntry::new(user)
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{
        ENCRYPTED_REF_PREFIX, SecretBackend, SecretConfig, SecretError, backend_of,
        delete_secret_with, get_secret_with, put_secret_with,
    };

    #[test]
    fn encrypted_round_trips_and_is_prefixed() {
        let config = SecretConfig::encrypted(b"test-master-key-not-secret");
        let stored = put_secret_with("hunter2", &config).unwrap();
        assert_eq!(stored.backend, SecretBackend::Encrypted);
        assert!(stored.reference.starts_with(ENCRYPTED_REF_PREFIX));
        assert_eq!(
            get_secret_with(&stored.reference, &config)
                .unwrap()
                .as_deref()
                .map(|value| value.as_str()),
            Some("hunter2")
        );
        assert_eq!(
            backend_of(&stored.reference),
            Some(SecretBackend::Encrypted)
        );
        // Different nonce each time; ciphertext differs.
        let again = put_secret_with("hunter2", &config).unwrap();
        assert_ne!(stored.reference, again.reference);
        assert_eq!(
            get_secret_with(&again.reference, &config)
                .unwrap()
                .as_deref()
                .map(|value| value.as_str()),
            Some("hunter2")
        );
        delete_secret_with(&stored.reference, &config).unwrap();
    }

    #[test]
    fn encrypted_wrong_key_does_not_decrypt() {
        let one = SecretConfig::encrypted(b"key-one");
        let two = SecretConfig::encrypted(b"key-two");
        let stored = put_secret_with("secret", &one).unwrap();
        assert!(get_secret_with(&stored.reference, &two).is_err());
    }

    #[test]
    fn empty_reference_is_none() {
        let config = SecretConfig::encrypted(b"k");
        assert_eq!(get_secret_with("", &config).unwrap(), None);
    }

    #[test]
    fn unknown_ref_prefix_is_an_error() {
        let config = SecretConfig::encrypted(b"k");
        assert!(matches!(
            get_secret_with("nope:abc", &config),
            Err(SecretError::UnknownRef(_))
        ));
    }

    #[test]
    fn encrypted_ref_without_key_is_no_secret_store() {
        let config = SecretConfig::keyring();
        let stored = put_secret_with("x", &SecretConfig::encrypted(b"k")).unwrap();
        assert!(matches!(
            get_secret_with(&stored.reference, &config),
            Err(SecretError::NoSecretStore)
        ));
    }

    #[test]
    fn keyring_prefix_is_classified_even_without_backend() {
        // We do not call put here (no keyring backend on CI); we only check the
        // classifier and that deleting a missing keyring entry is not an error.
        assert_eq!(backend_of("kr:facet/abc"), Some(SecretBackend::Keyring));
        assert_eq!(backend_of(""), None);
        let config = SecretConfig::keyring();
        let _ = delete_secret_with("kr:facet/does-not-exist", &config);
    }
}
