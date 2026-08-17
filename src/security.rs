use crate::ast::Statement;
use crate::error::{FluxError, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use pbkdf2::pbkdf2_hmac;
use rand::{RngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const PBKDF2_ROUNDS: u32 = 210_000;
const SALT_LEN: usize = 16;
const HASH_LEN: usize = 32;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Admin,
    ReadWrite,
    ReadOnly,
}

impl Role {
    pub fn parse(input: &str) -> Result<Self> {
        match input.trim().to_ascii_lowercase().as_str() {
            "admin" => Ok(Self::Admin),
            "read_write" | "readwrite" | "rw" => Ok(Self::ReadWrite),
            "read_only" | "readonly" | "ro" => Ok(Self::ReadOnly),
            _ => Err(FluxError::Configuration(format!(
                "unknown role '{input}' (expected admin|read_write|read_only)"
            ))),
        }
    }

    pub fn allows(&self, statement: &Statement) -> bool {
        match self {
            Self::Admin => true,
            Self::ReadWrite => matches!(
                statement,
                Statement::Insert { .. }
                    | Statement::Update { .. }
                    | Statement::Select { .. }
                    | Statement::Delete { .. }
                    | Statement::Begin
                    | Statement::Commit
                    | Statement::Rollback
                    | Statement::ShowTables
                    | Statement::ShowMigrations
                    | Statement::Describe { .. }
            ),
            Self::ReadOnly => matches!(
                statement,
                Statement::Select { .. }
                    | Statement::ShowTables
                    | Statement::ShowMigrations
                    | Statement::Describe { .. }
            ),
        }
    }
}

impl Display for Role {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Admin => write!(f, "admin"),
            Self::ReadWrite => write!(f, "read_write"),
            Self::ReadOnly => write!(f, "read_only"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub username: String,
    pub role: Role,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UserRecord {
    role: Role,
    salt_b64: String,
    hash_b64: String,
    iterations: u32,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct AuthStore {
    users: BTreeMap<String, UserRecord>,
}

pub struct AuthManager {
    path: PathBuf,
    store: AuthStore,
    crypto: CryptoManager,
}

impl AuthManager {
    pub fn open(data_dir: &Path, crypto: CryptoManager) -> Result<Self> {
        let security_dir = data_dir.join("security");
        fs::create_dir_all(&security_dir)?;
        let path = security_dir.join("users.enc");
        let store = if path.exists() {
            let encrypted = fs::read_to_string(&path)?;
            if encrypted.trim().is_empty() {
                AuthStore::default()
            } else {
                let decrypted = crypto.decrypt_from_base64(&encrypted)?;
                serde_json::from_slice::<AuthStore>(&decrypted)?
            }
        } else {
            AuthStore::default()
        };
        Ok(Self {
            path,
            store,
            crypto,
        })
    }

    pub fn has_users(&self) -> bool {
        !self.store.users.is_empty()
    }

    pub fn create_user(&mut self, username: &str, password: &str, role: Role) -> Result<()> {
        validate_username(username)?;
        if password.len() < 12 {
            return Err(FluxError::Configuration(
                "password must be at least 12 characters long".to_string(),
            ));
        }
        if self.store.users.contains_key(username) {
            return Err(FluxError::UserExists(username.to_string()));
        }

        let mut salt = [0u8; SALT_LEN];
        OsRng.fill_bytes(&mut salt);
        let mut hash = [0u8; HASH_LEN];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, PBKDF2_ROUNDS, &mut hash);

        self.store.users.insert(
            username.to_string(),
            UserRecord {
                role,
                salt_b64: BASE64_STANDARD.encode(salt),
                hash_b64: BASE64_STANDARD.encode(hash),
                iterations: PBKDF2_ROUNDS,
            },
        );
        self.save()
    }

    pub fn authenticate(&self, username: &str, password: &str) -> Result<Identity> {
        let record = self
            .store
            .users
            .get(username)
            .ok_or(FluxError::AuthenticationFailed)?;

        let salt = BASE64_STANDARD.decode(&record.salt_b64)?;
        let expected_hash = BASE64_STANDARD.decode(&record.hash_b64)?;
        let mut computed = vec![0u8; expected_hash.len()];
        pbkdf2_hmac::<Sha256>(password.as_bytes(), &salt, record.iterations, &mut computed);

        if !constant_time_eq(&computed, &expected_hash) {
            return Err(FluxError::AuthenticationFailed);
        }

        Ok(Identity {
            username: username.to_string(),
            role: record.role.clone(),
        })
    }

    fn save(&self) -> Result<()> {
        let serialized = serde_json::to_vec_pretty(&self.store)?;
        let encrypted = self.crypto.encrypt_to_base64(&serialized)?;
        let tmp = self.path.with_extension("tmp");
        fs::write(&tmp, &encrypted)?;
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(tmp, &self.path)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct CryptoManager {
    cipher: XChaCha20Poly1305,
}

impl CryptoManager {
    pub fn from_env(env_name: &str) -> Result<Self> {
        let key_b64 = std::env::var(env_name).map_err(|_| {
            FluxError::Configuration(format!("master key env var '{env_name}' is not set"))
        })?;
        Self::from_base64_key(&key_b64)
    }

    pub fn from_base64_key(key_b64: &str) -> Result<Self> {
        let key_raw = BASE64_STANDARD.decode(key_b64.trim())?;
        if key_raw.len() != KEY_LEN {
            return Err(FluxError::Configuration(format!(
                "invalid master key length: expected {KEY_LEN} bytes after base64 decoding"
            )));
        }
        let key = Key::from_slice(&key_raw);
        Ok(Self {
            cipher: XChaCha20Poly1305::new(key),
        })
    }

    pub fn generate_base64_key() -> String {
        let mut key = [0u8; KEY_LEN];
        OsRng.fill_bytes(&mut key);
        BASE64_STANDARD.encode(key)
    }

    pub fn encrypt_to_base64(&self, plaintext: &[u8]) -> Result<String> {
        let mut nonce_bytes = [0u8; NONCE_LEN];
        OsRng.fill_bytes(&mut nonce_bytes);

        let nonce = XNonce::from_slice(&nonce_bytes);
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|_| FluxError::Crypto("encryption failed".to_string()))?;

        let mut payload = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        payload.extend_from_slice(&nonce_bytes);
        payload.extend_from_slice(&ciphertext);
        Ok(BASE64_STANDARD.encode(payload))
    }

    pub fn decrypt_from_base64(&self, encoded: &str) -> Result<Vec<u8>> {
        let payload = BASE64_STANDARD.decode(encoded.trim())?;
        if payload.len() <= NONCE_LEN {
            return Err(FluxError::Crypto(
                "encrypted payload is too short".to_string(),
            ));
        }

        let (nonce_bytes, ciphertext) = payload.split_at(NONCE_LEN);
        let nonce = XNonce::from_slice(nonce_bytes);
        self.cipher
            .decrypt(nonce, ciphertext)
            .map_err(|_| FluxError::Crypto("decryption failed".to_string()))
    }
}

#[derive(Debug, Serialize)]
struct AuditEntry<'a> {
    timestamp_unix_ms: u128,
    username: &'a str,
    role: &'a Role,
    action: &'a str,
    success: bool,
    message: Option<&'a str>,
}

pub struct AuditLogger {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl AuditLogger {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let security_dir = data_dir.join("security");
        fs::create_dir_all(&security_dir)?;
        Ok(Self {
            path: security_dir.join("audit.log"),
            file: Mutex::new(None),
        })
    }

    pub fn log(
        &self,
        identity: &Identity,
        action: &str,
        success: bool,
        message: Option<&str>,
    ) -> Result<()> {
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| FluxError::Configuration("system clock error".to_string()))?
            .as_millis();
        let entry = AuditEntry {
            timestamp_unix_ms,
            username: &identity.username,
            role: &identity.role,
            action,
            success,
            message,
        };
        let line = serde_json::to_string(&entry)?;
        let mut guard = self
            .file
            .lock()
            .map_err(|_| FluxError::Configuration("audit log lock poisoned".to_string()))?;
        if guard.is_none() {
            *guard = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.path)?,
            );
        }
        let file = guard
            .as_mut()
            .ok_or_else(|| FluxError::Configuration("audit log unavailable".to_string()))?;
        writeln!(file, "{line}")?;
        Ok(())
    }
}

pub fn statement_action(statement: &Statement) -> &'static str {
    match statement {
        Statement::CreateTable { .. } => "CREATE_TABLE",
        Statement::DropTable { .. } => "DROP_TABLE",
        Statement::CreateIndex { .. } => "CREATE_INDEX",
        Statement::DropIndex { .. } => "DROP_INDEX",
        Statement::AlterTableAddColumn { .. } => "ALTER_TABLE_ADD_COLUMN",
        Statement::AlterTableDropColumn { .. } => "ALTER_TABLE_DROP_COLUMN",
        Statement::AlterTableRenameColumn { .. } => "ALTER_TABLE_RENAME_COLUMN",
        Statement::Insert { .. } => "INSERT",
        Statement::Update { .. } => "UPDATE",
        Statement::Select { .. } => "SELECT",
        Statement::Delete { .. } => "DELETE",
        Statement::Begin => "BEGIN",
        Statement::Commit => "COMMIT",
        Statement::Rollback => "ROLLBACK",
        Statement::ShowTables => "SHOW_TABLES",
        Statement::ShowMigrations => "SHOW_MIGRATIONS",
        Statement::Describe { .. } => "DESCRIBE",
    }
}

fn validate_username(username: &str) -> Result<()> {
    if username.is_empty() {
        return Err(FluxError::Configuration(
            "username cannot be empty".to_string(),
        ));
    }
    if !username
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(FluxError::Configuration(
            "username can contain only ASCII letters, numbers and '_'".to_string(),
        ));
    }
    Ok(())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn auth_roundtrip() {
        let tmp = tempdir().expect("tempdir");
        let key = CryptoManager::generate_base64_key();
        let crypto = CryptoManager::from_base64_key(&key).expect("key parse");
        let mut auth = AuthManager::open(tmp.path(), crypto.clone()).expect("auth open");
        auth.create_user("admin", "very_secure_password", Role::Admin)
            .expect("create user");
        let identity = auth
            .authenticate("admin", "very_secure_password")
            .expect("auth should pass");
        assert_eq!(identity.username, "admin");
        assert_eq!(identity.role, Role::Admin);

        let auth2 = AuthManager::open(tmp.path(), crypto).expect("reopen");
        let identity2 = auth2
            .authenticate("admin", "very_secure_password")
            .expect("auth should pass after reopen");
        assert_eq!(identity2.role, Role::Admin);
    }

    #[test]
    fn crypto_roundtrip() {
        let key = CryptoManager::generate_base64_key();
        let crypto = CryptoManager::from_base64_key(&key).expect("key should parse");
        let encrypted = crypto
            .encrypt_to_base64(b"secret payload")
            .expect("encrypt");
        let decrypted = crypto
            .decrypt_from_base64(&encrypted)
            .expect("decrypt should work");
        assert_eq!(decrypted, b"secret payload");
    }
}
