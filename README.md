# FluxDB

A tiny PostgreSQL-inspired SQL database written in Rust. All data is encrypted at rest using XChaCha20-Poly1305, with built-in user authentication (PBKDF2), role-based access control, and audit logging.

## Features

- **Encrypted storage** — every row, catalog, index, and migration record is encrypted with XChaCha20-Poly1305
- **Authentication** — password-based user management with PBKDF2-HMAC-SHA256 (210 000 iterations)
- **Role-based access control** — three roles: `admin`, `read_write`, `read_only`
- **Audit log** — every query is logged with timestamp, user, action, and result
- **Indexes** — B-tree indexes on individual columns for faster equality lookups
- **Migrations** — schema changes are tracked automatically (`SHOW MIGRATIONS`)
- **Interactive REPL** — or batch mode via `--execute` / `--file`

## Quick start

### 1. Build

```bash
cargo build --release
```

### 2. Generate a master encryption key

```bash
fluxdb --keygen
```

Set the key as an environment variable:

```bash
export FLUXDB_MASTER_KEY="<generated key>"
```

### 3. Bootstrap the first admin user

```bash
fluxdb --bootstrap-admin admin --user admin --password-stdin
```

You will be prompted for a password (minimum 12 characters).

### 4. Start the REPL

```bash
fluxdb --user admin
```

## SQL reference

### Data types

| Type | Aliases |
|------|---------|
| `INT` | `INTEGER` |
| `TEXT` | `STRING` |
| `BOOL` | `BOOLEAN` |

### Supported statements

```sql
-- Tables
CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL UNIQUE, active BOOL);
DROP TABLE users;
DESCRIBE users;
SHOW TABLES;

-- Data
INSERT INTO users VALUES (1, 'Alice', true);
INSERT INTO users (id, name) VALUES (2, 'Bob');
SELECT * FROM users;
SELECT id, name FROM users WHERE active = true;
SELECT COUNT(*), SUM(id), AVG(id), MIN(id), MAX(id) FROM users;
SELECT id, name FROM users ORDER BY id DESC LIMIT 10 OFFSET 5;
SELECT users.id, orders.id FROM users JOIN orders ON users.id = orders.user_id;
UPDATE users SET name = 'Bob' WHERE id = 1;
DELETE FROM users WHERE id = 1;

-- Transactions
BEGIN;
COMMIT;
ROLLBACK;

-- Indexes
CREATE INDEX idx_users_id ON users(id);
DROP INDEX idx_users_id;

-- Schema migrations
ALTER TABLE users ADD COLUMN email TEXT DEFAULT 'N/A';
ALTER TABLE users DROP COLUMN email;
ALTER TABLE users RENAME COLUMN name TO full_name;
SHOW MIGRATIONS;
```

### Column constraints

`PRIMARY KEY`, `NOT NULL`, `UNIQUE` — set at `CREATE TABLE` time.

### Aggregates

`COUNT(*)`, `SUM(col)`, `AVG(col)`, `MIN(col)`, `MAX(col)`.

### JOIN

Single inner `JOIN ... ON` per query, e.g. `SELECT * FROM users JOIN orders ON users.id = orders.user_id`.

### ORDER BY / LIMIT / OFFSET

```sql
SELECT * FROM users ORDER BY id DESC LIMIT 10 OFFSET 5;
```

### Transactions

`BEGIN`, `COMMIT`, `ROLLBACK` wrap a batch of statements.

### NULL handling

```sql
SELECT * FROM users WHERE email IS NULL;
SELECT * FROM users WHERE email IS NOT NULL;
```

### WHERE clause

Supports `=`, `!=`, `<>`, `>`, `>=`, `<`, `<=`, `LIKE`, combined with `AND` / `OR`.

```sql
SELECT * FROM users WHERE id >= 10 AND name LIKE 'A%';
SELECT * FROM users WHERE active = true OR name = 'admin';
```

## CLI options

| Flag | Description |
|------|-------------|
| `--data-dir <path>` | Data directory (default: `./data`) |
| `--keygen` | Print a new base64 master key and exit |
| `--master-key-env <var>` | Env var holding the master key (default: `FLUXDB_MASTER_KEY`) |
| `--user <name>` | Username for login |
| `--password-env <var>` | Env var with login password (default: `FLUXDB_PASSWORD`) |
| `--password-stdin` | Read login password from stdin |
| `--bootstrap-admin <name>` | Create the first admin user |
| `--add-user <name>` | Admin-only: create a new user |
| `--add-role <role>` | Role for `--add-user` (`admin`, `read_write`, `read_only`) |
| `-e, --execute <sql>` | Execute SQL string and exit |
| `-f, --file <path>` | Execute SQL file and exit |

## Roles

| Role | Permissions |
|------|-------------|
| `admin` | All operations |
| `read_write` | `INSERT`, `UPDATE`, `DELETE`, `SELECT`, `SHOW TABLES`, `SHOW MIGRATIONS`, `DESCRIBE` |
| `read_only` | `SELECT`, `SHOW TABLES`, `SHOW MIGRATIONS`, `DESCRIBE` |

## Running tests

```bash
cargo test
```

## Running benchmarks

```bash
cargo bench
```

## License

This project is provided as-is for educational purposes.
