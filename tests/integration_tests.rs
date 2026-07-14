#[cfg(test)]
mod tests {
    use fluxdb::engine::Engine;
    use fluxdb::security::{AuditLogger, AuthManager, CryptoManager, Identity, Role};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_create_database_and_table() {
        let tmp_dir = TempDir::new().unwrap();
        let crypto = CryptoManager::generate_base64_key();
        let crypto_manager = CryptoManager::from_base64_key(&crypto).unwrap();
        let audit_logger = AuditLogger::open(tmp_dir.path()).unwrap();

        let identity = Identity {
            username: "test_user".to_string(),
            role: Role::Admin,
        };

        let mut engine = Engine::open(tmp_dir.path(), crypto_manager, identity, audit_logger).unwrap();

        let result = engine.execute_script("CREATE TABLE users (id INT, name TEXT, active BOOL);");
        assert!(result.is_ok());

        let result = engine.execute_script("INSERT INTO users VALUES (1, 'John Doe', true);");
        assert!(result.is_ok());

        let result = engine.execute_script("SELECT * FROM users;");
        assert!(result.is_ok());
    }

    #[test]
    fn test_user_authentication() {
        let tmp_dir = TempDir::new().unwrap();
        let crypto = CryptoManager::generate_base64_key();
        let crypto_manager = CryptoManager::from_base64_key(&crypto).unwrap();

        let mut auth_manager = AuthManager::open(tmp_dir.path(), crypto_manager.clone()).unwrap();

        let result = auth_manager.create_user("test_user", "secure_password_123", Role::ReadWrite);
        assert!(result.is_ok());

        let identity = auth_manager.authenticate("test_user", "secure_password_123");
        assert!(identity.is_ok());
        assert_eq!(identity.unwrap().role, Role::ReadWrite);
    }

    #[test]
    fn test_encryption_decryption() {
        let crypto = CryptoManager::generate_base64_key();
        let crypto_manager = CryptoManager::from_base64_key(&crypto).unwrap();

        let original_data = b"Test data for encryption";
        let encrypted = crypto_manager.encrypt_to_base64(original_data).unwrap();
        let decrypted = crypto_manager.decrypt_from_base64(&encrypted).unwrap();

        assert_eq!(original_data, &decrypted[..]);
    }

    #[test]
    fn test_sql_operations() {
        let tmp_dir = TempDir::new().unwrap();
        let crypto = CryptoManager::generate_base64_key();
        let crypto_manager = CryptoManager::from_base64_key(&crypto).unwrap();
        let audit_logger = AuditLogger::open(tmp_dir.path()).unwrap();

        let identity = Identity {
            username: "test_user".to_string(),
            role: Role::Admin,
        };

        let mut engine = Engine::open(tmp_dir.path(), crypto_manager, identity, audit_logger).unwrap();

        let result = engine.execute_script("CREATE TABLE products (id INT, name TEXT, price INT);");
        assert!(result.is_ok());

        let result = engine.execute_script("INSERT INTO products VALUES (1, 'Laptop', 1000);");
        assert!(result.is_ok());

        let result = engine.execute_script("SELECT * FROM products;");
        assert!(result.is_ok());

        let result = engine.execute_script("UPDATE products SET price = 900 WHERE id = 1;");
        assert!(result.is_ok());

        let result = engine.execute_script("DELETE FROM products WHERE id = 1;");
        assert!(result.is_ok());
    }
}