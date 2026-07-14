use crate::error::{FluxError, Result};
use crate::security::CryptoManager;
use crate::types::{
    Catalog, ColumnSchema, IndexCatalog, IndexDefinition, MigrationRecord, Row, StoredRow,
    TableSchema, Value,
};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Storage {
    root: PathBuf,
    catalog: Catalog,
    crypto: CryptoManager,
    migrations: Vec<MigrationRecord>,
    index_catalog: IndexCatalog,
    txn_dir: Option<PathBuf>,
}

impl Storage {
    pub fn open(root: impl AsRef<Path>, crypto: CryptoManager) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(&root)?;

        let catalog = load_encrypted_json(&root.join("catalog.enc"), &crypto, Catalog::default())?;
        let migrations = load_encrypted_json(
            &root.join("migrations.enc"),
            &crypto,
            Vec::<MigrationRecord>::new(),
        )?;
        let index_catalog =
            load_encrypted_json(&root.join("indexes.enc"), &crypto, IndexCatalog::default())?;

        let txn_path = root.join(".txn_snapshot");
        if txn_path.exists() {
            let _ = fs::remove_dir_all(&txn_path);
        }

        Ok(Self {
            root,
            catalog,
            crypto,
            migrations,
            index_catalog,
            txn_dir: None,
        })
    }


    pub fn begin_transaction(&mut self) -> Result<()> {
        if self.txn_dir.is_some() {
            return Err(FluxError::Transaction(
                "transaction already in progress".to_string(),
            ));
        }

        let txn_path = self.root.join(".txn_snapshot");
        if txn_path.exists() {
            fs::remove_dir_all(&txn_path)?;
        }
        fs::create_dir_all(&txn_path)?;

        copy_if_exists(&self.catalog_path(), &txn_path.join("catalog.enc"))?;
        copy_if_exists(&self.migrations_path(), &txn_path.join("migrations.enc"))?;
        copy_if_exists(&self.index_catalog_path(), &txn_path.join("indexes.enc"))?;

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".table") || name_str.starts_with("index_") {
                copy_if_exists(&entry.path(), &txn_path.join(&*name))?;
            }
        }

        self.txn_dir = Some(txn_path);
        Ok(())
    }

    pub fn commit_transaction(&mut self) -> Result<()> {
        let txn_path = self
            .txn_dir
            .take()
            .ok_or_else(|| FluxError::Transaction("no transaction in progress".to_string()))?;
        if txn_path.exists() {
            fs::remove_dir_all(&txn_path)?;
        }
        Ok(())
    }

    pub fn rollback_transaction(&mut self) -> Result<()> {
        let txn_path = self
            .txn_dir
            .take()
            .ok_or_else(|| FluxError::Transaction("no transaction in progress".to_string()))?;

        if !txn_path.exists() {
            return Err(FluxError::Transaction(
                "transaction snapshot missing".to_string(),
            ));
        }

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name_str = entry.file_name().to_string_lossy().to_string();
            if name_str.ends_with(".table")
                || name_str.starts_with("index_")
                || name_str == "catalog.enc"
                || name_str == "migrations.enc"
                || name_str == "indexes.enc"
            {
                fs::remove_file(entry.path())?;
            }
        }

        for entry in fs::read_dir(&txn_path)? {
            let entry = entry?;
            let dest = self.root.join(entry.file_name());
            fs::copy(entry.path(), &dest)?;
        }
        fs::remove_dir_all(&txn_path)?;

        self.catalog =
            load_encrypted_json(&self.catalog_path(), &self.crypto, Catalog::default())?;
        self.migrations = load_encrypted_json(
            &self.migrations_path(),
            &self.crypto,
            Vec::<MigrationRecord>::new(),
        )?;
        self.index_catalog = load_encrypted_json(
            &self.index_catalog_path(),
            &self.crypto,
            IndexCatalog::default(),
        )?;

        Ok(())
    }


    pub fn create_table(&mut self, schema: TableSchema) -> Result<()> {
        if self.catalog.tables.contains_key(&schema.name) {
            return Err(FluxError::TableExists(schema.name));
        }

        self.catalog
            .tables
            .insert(schema.name.clone(), schema.clone());
        File::create(self.table_path(&schema.name))?;
        self.save_catalog()?;

        let details = schema
            .columns
            .iter()
            .map(|column| {
                let constraints = column
                    .constraints
                    .iter()
                    .map(|c| format!(" {c}"))
                    .collect::<String>();
                format!("{} {}{}", column.name, column.data_type, constraints)
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.record_migration(
            &schema.name,
            "CREATE_TABLE",
            &format!("CREATE TABLE {} ({details})", schema.name),
        )?;
        Ok(())
    }

    pub fn drop_table(&mut self, table: &str) -> Result<()> {
        if !self.catalog.tables.contains_key(table) {
            return Err(FluxError::TableNotFound(table.to_string()));
        }

        let related_indexes: Vec<String> = self
            .index_catalog
            .indexes
            .iter()
            .filter(|idx| idx.table_name == table)
            .map(|idx| idx.name.clone())
            .collect();
        for idx_name in &related_indexes {
            let path = self.index_path(idx_name);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        self.index_catalog
            .indexes
            .retain(|idx| idx.table_name != table);
        self.save_index_catalog()?;

        let table_path = self.table_path(table);
        if table_path.exists() {
            fs::remove_file(table_path)?;
        }

        self.catalog.tables.remove(table);
        self.save_catalog()?;
        self.record_migration(table, "DROP_TABLE", &format!("DROP TABLE {table}"))?;
        Ok(())
    }

    pub fn create_index(&mut self, index_name: &str, table: &str, column: &str) -> Result<usize> {
        let schema = self
            .catalog
            .tables
            .get(table)
            .ok_or_else(|| FluxError::TableNotFound(table.to_string()))?;
        if !schema.columns.iter().any(|col| col.name == column) {
            return Err(FluxError::ColumnNotFound {
                table: table.to_string(),
                column: column.to_string(),
            });
        }
        if self
            .index_catalog
            .indexes
            .iter()
            .any(|idx| idx.name == index_name)
        {
            return Err(FluxError::InvalidSchema(format!(
                "index '{index_name}' already exists"
            )));
        }
        if self
            .index_catalog
            .indexes
            .iter()
            .any(|idx| idx.table_name == table && idx.column_name == column)
        {
            return Err(FluxError::InvalidSchema(format!(
                "table '{table}' already has an index on column '{column}'"
            )));
        }

        self.index_catalog.indexes.push(IndexDefinition {
            name: index_name.to_string(),
            table_name: table.to_string(),
            column_name: column.to_string(),
        });
        self.save_index_catalog()?;

        let keys = self.rebuild_index(index_name)?;
        self.record_migration(
            table,
            "CREATE_INDEX",
            &format!("CREATE INDEX {index_name} ON {table}({column})"),
        )?;
        Ok(keys)
    }

    pub fn drop_index(&mut self, index_name: &str) -> Result<String> {
        let idx = self
            .index_catalog
            .indexes
            .iter()
            .find(|idx| idx.name == index_name)
            .ok_or_else(|| {
                FluxError::InvalidSchema(format!("index '{index_name}' not found"))
            })?;
        let table = idx.table_name.clone();

        let path = self.index_path(index_name);
        if path.exists() {
            fs::remove_file(path)?;
        }

        self.index_catalog
            .indexes
            .retain(|idx| idx.name != index_name);
        self.save_index_catalog()?;
        self.record_migration(
            &table,
            "DROP_INDEX",
            &format!("DROP INDEX {index_name}"),
        )?;
        Ok(table)
    }

    pub fn find_indexed_row_ids(
        &self,
        table: &str,
        column: &str,
        value: &Value,
    ) -> Result<Option<Vec<u64>>> {
        let Some(index) = self
            .index_catalog
            .indexes
            .iter()
            .find(|idx| idx.table_name == table && idx.column_name == column)
        else {
            return Ok(None);
        };

        let map = self.read_index_map(&index.name)?;
        Ok(map.get(&index_value_key(value)?).cloned())
    }

    pub fn add_column(
        &mut self,
        table: &str,
        column: ColumnSchema,
        default: Value,
    ) -> Result<usize> {
        {
            let schema = self
                .catalog
                .tables
                .get_mut(table)
                .ok_or_else(|| FluxError::TableNotFound(table.to_string()))?;

            if schema.columns.iter().any(|col| col.name == column.name) {
                return Err(FluxError::InvalidSchema(format!(
                    "column '{}' already exists in table '{}'",
                    column.name, table
                )));
            }
            schema.columns.push(column.clone());
        }

        self.save_catalog()?;

        let mut rows = self.read_stored_rows(table)?;
        let updated_rows = rows.len();
        for stored in &mut rows {
            stored.data.insert(column.name.clone(), default.clone());
        }
        self.write_stored_rows(table, &rows)?;

        self.record_migration(
            table,
            "ALTER_TABLE_ADD_COLUMN",
            &format!(
                "ALTER TABLE {table} ADD COLUMN {} {} DEFAULT {}",
                column.name, column.data_type, default
            ),
        )?;
        Ok(updated_rows)
    }

    pub fn drop_column(&mut self, table: &str, column: &str) -> Result<usize> {
        {
            let schema = self
                .catalog
                .tables
                .get_mut(table)
                .ok_or_else(|| FluxError::TableNotFound(table.to_string()))?;
            if schema.columns.len() <= 1 {
                return Err(FluxError::InvalidSchema(
                    "cannot drop the last column of a table".to_string(),
                ));
            }
            if !schema.columns.iter().any(|col| col.name == column) {
                return Err(FluxError::ColumnNotFound {
                    table: table.to_string(),
                    column: column.to_string(),
                });
            }
            schema.columns.retain(|col| col.name != column);
        }
        self.save_catalog()?;

        let to_remove: Vec<String> = self
            .index_catalog
            .indexes
            .iter()
            .filter(|idx| idx.table_name == table && idx.column_name == column)
            .map(|idx| idx.name.clone())
            .collect();
        for idx_name in &to_remove {
            let path = self.index_path(idx_name);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        if !to_remove.is_empty() {
            self.index_catalog
                .indexes
                .retain(|idx| !(idx.table_name == table && idx.column_name == column));
            self.save_index_catalog()?;
        }

        let mut rows = self.read_stored_rows(table)?;
        let count = rows.len();
        for stored in &mut rows {
            stored.data.remove(column);
        }
        self.write_stored_rows(table, &rows)?;

        self.record_migration(
            table,
            "ALTER_TABLE_DROP_COLUMN",
            &format!("ALTER TABLE {table} DROP COLUMN {column}"),
        )?;
        Ok(count)
    }

    pub fn rename_column(&mut self, table: &str, old_name: &str, new_name: &str) -> Result<usize> {
        {
            let schema = self
                .catalog
                .tables
                .get_mut(table)
                .ok_or_else(|| FluxError::TableNotFound(table.to_string()))?;
            if !schema.columns.iter().any(|col| col.name == old_name) {
                return Err(FluxError::ColumnNotFound {
                    table: table.to_string(),
                    column: old_name.to_string(),
                });
            }
            if schema.columns.iter().any(|col| col.name == new_name) {
                return Err(FluxError::InvalidSchema(format!(
                    "column '{new_name}' already exists in table '{table}'"
                )));
            }
            for col in &mut schema.columns {
                if col.name == old_name {
                    col.name = new_name.to_string();
                }
            }
        }
        self.save_catalog()?;

        for idx in &mut self.index_catalog.indexes {
            if idx.table_name == table && idx.column_name == old_name {
                idx.column_name = new_name.to_string();
            }
        }
        self.save_index_catalog()?;

        let mut rows = self.read_stored_rows(table)?;
        let count = rows.len();
        for stored in &mut rows {
            if let Some(value) = stored.data.remove(old_name) {
                stored.data.insert(new_name.to_string(), value);
            }
        }
        self.write_stored_rows(table, &rows)?;
        self.rebuild_indexes_for_table(table)?;

        self.record_migration(
            table,
            "ALTER_TABLE_RENAME_COLUMN",
            &format!("ALTER TABLE {table} RENAME COLUMN {old_name} TO {new_name}"),
        )?;
        Ok(count)
    }

    pub fn list_tables(&self) -> Vec<String> {
        self.catalog.tables.keys().cloned().collect()
    }

    pub fn list_migrations(&self) -> Vec<MigrationRecord> {
        self.migrations.clone()
    }

    pub fn get_schema(&self, table: &str) -> Result<TableSchema> {
        self.catalog
            .tables
            .get(table)
            .cloned()
            .ok_or_else(|| FluxError::TableNotFound(table.to_string()))
    }

    pub fn append_row(&mut self, table: &str, row: &Row) -> Result<()> {
        let row_id = {
            let schema = self
                .catalog
                .tables
                .get_mut(table)
                .ok_or_else(|| FluxError::TableNotFound(table.to_string()))?;
            let id = schema.next_row_id;
            schema.next_row_id += 1;
            id
        };
        self.save_catalog()?;

        let stored = StoredRow {
            id: row_id,
            data: row.clone(),
        };
        let payload = serde_json::to_vec(&stored)?;
        let encrypted = self.crypto.encrypt_to_base64(&payload)?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.table_path(table))?;
        writeln!(file, "{encrypted}")?;

        let index_names: Vec<(String, String)> = self
            .index_catalog
            .indexes
            .iter()
            .filter(|idx| idx.table_name == table)
            .map(|idx| (idx.name.clone(), idx.column_name.clone()))
            .collect();
        for (index_name, column_name) in index_names {
            let value = row.get(&column_name).cloned().unwrap_or(Value::Null);
            let mut map = self.read_index_map(&index_name)?;
            map.entry(index_value_key(&value)?).or_default().push(row_id);
            self.write_index_map(&index_name, &map)?;
        }
        Ok(())
    }

    pub fn read_rows(&self, table: &str) -> Result<Vec<Row>> {
        Ok(self
            .read_stored_rows(table)?
            .into_iter()
            .map(|stored| stored.data)
            .collect())
    }

    pub fn read_rows_filtered(&self, table: &str, row_ids: Option<&[u64]>) -> Result<Vec<Row>> {
        let Some(wanted) = row_ids else {
            return self.read_rows(table);
        };
        let wanted: std::collections::HashSet<u64> = wanted.iter().copied().collect();
        Ok(self
            .read_stored_rows(table)?
            .into_iter()
            .filter(|stored| wanted.contains(&stored.id))
            .map(|stored| stored.data)
            .collect())
    }

    fn read_stored_rows(&self, table: &str) -> Result<Vec<StoredRow>> {
        if !self.catalog.tables.contains_key(table) {
            return Err(FluxError::TableNotFound(table.to_string()));
        }

        let table_path = self.table_path(table);
        if !table_path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(table_path)?;
        let reader = BufReader::new(file);
        let mut rows = Vec::new();
        for line in reader.lines() {
            let encrypted_line = line?;
            if encrypted_line.trim().is_empty() {
                continue;
            }
            let decrypted = self.crypto.decrypt_from_base64(&encrypted_line)?;
            rows.push(serde_json::from_slice::<StoredRow>(&decrypted)?);
        }
        Ok(rows)
    }

    pub fn rewrite_rows<F>(&mut self, table: &str, mut mapper: F) -> Result<usize>
    where
        F: FnMut(Row) -> Result<Option<Row>>,
    {
        if !self.catalog.tables.contains_key(table) {
            return Err(FluxError::TableNotFound(table.to_string()));
        }
        let input_path = self.table_path(table);
        let tmp_path = self.table_path(&format!("{table}.tmp_rewrite"));

        let input_exists = input_path.exists();
        let mut output = File::create(&tmp_path)?;
        let mut affected = 0usize;

        if input_exists {
            let file = File::open(&input_path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let encrypted_line = line?;
                if encrypted_line.trim().is_empty() {
                    continue;
                }
                let decrypted = self.crypto.decrypt_from_base64(&encrypted_line)?;
                let stored = serde_json::from_slice::<StoredRow>(&decrypted)?;
                let id = stored.id;
                let mapped = mapper(stored.data)?;
                if let Some(data) = mapped {
                    let payload = serde_json::to_vec(&StoredRow { id, data })?;
                    let encrypted = self.crypto.encrypt_to_base64(&payload)?;
                    writeln!(output, "{encrypted}")?;
                } else {
                    affected += 1;
                }
            }
        }

        output.sync_all()?;
        drop(output);
        if input_exists {
            fs::remove_file(&input_path)?;
        }
        fs::rename(&tmp_path, &input_path)?;
        self.rebuild_indexes_for_table(table)?;
        Ok(affected)
    }

    fn write_stored_rows(&mut self, table: &str, rows: &[StoredRow]) -> Result<()> {
        if !self.catalog.tables.contains_key(table) {
            return Err(FluxError::TableNotFound(table.to_string()));
        }

        let mut buf = Vec::new();
        for stored in rows {
            let payload = serde_json::to_vec(stored)?;
            let encrypted = self.crypto.encrypt_to_base64(&payload)?;
            buf.extend_from_slice(encrypted.as_bytes());
            buf.push(b'\n');
        }
        atomic_write(&self.table_path(table), &buf)?;
        self.rebuild_indexes_for_table(table)?;
        Ok(())
    }

    fn record_migration(&mut self, table_name: &str, operation: &str, details: &str) -> Result<()> {
        let id = self
            .migrations
            .last()
            .map(|entry| entry.id + 1)
            .unwrap_or(1);
        let executed_at_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| FluxError::Configuration("system clock error".to_string()))?
            .as_millis();

        self.migrations.push(MigrationRecord {
            id,
            executed_at_unix_ms,
            table_name: table_name.to_string(),
            operation: operation.to_string(),
            details: details.to_string(),
        });
        self.save_migrations()?;
        Ok(())
    }

    fn rebuild_indexes_for_table(&self, table: &str) -> Result<()> {
        for index in self
            .index_catalog
            .indexes
            .iter()
            .filter(|idx| idx.table_name == table)
        {
            self.rebuild_index(&index.name)?;
        }
        Ok(())
    }

    fn rebuild_index(&self, index_name: &str) -> Result<usize> {
        let index_def = self
            .index_catalog
            .indexes
            .iter()
            .find(|idx| idx.name == index_name)
            .ok_or_else(|| FluxError::InvalidSchema(format!("index '{index_name}' not found")))?;

        let rows = self.read_stored_rows(&index_def.table_name)?;
        let mut map: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for stored in &rows {
            let value = stored
                .data
                .get(&index_def.column_name)
                .cloned()
                .unwrap_or(Value::Null);
            map.entry(index_value_key(&value)?)
                .or_default()
                .push(stored.id);
        }
        self.write_index_map(index_name, &map)?;
        Ok(map.len())
    }

    fn read_index_map(&self, index_name: &str) -> Result<BTreeMap<String, Vec<u64>>> {
        load_encrypted_json(
            &self.index_path(index_name),
            &self.crypto,
            BTreeMap::<String, Vec<u64>>::new(),
        )
    }

    fn write_index_map(&self, index_name: &str, map: &BTreeMap<String, Vec<u64>>) -> Result<()> {
        save_encrypted_json(&self.index_path(index_name), &self.crypto, map)
    }

    fn save_catalog(&self) -> Result<()> {
        save_encrypted_json(&self.catalog_path(), &self.crypto, &self.catalog)
    }

    fn save_migrations(&self) -> Result<()> {
        save_encrypted_json(&self.migrations_path(), &self.crypto, &self.migrations)
    }

    fn save_index_catalog(&self) -> Result<()> {
        save_encrypted_json(
            &self.index_catalog_path(),
            &self.crypto,
            &self.index_catalog,
        )
    }

    fn table_path(&self, table: &str) -> PathBuf {
        self.root.join(format!("{table}.table"))
    }

    fn catalog_path(&self) -> PathBuf {
        self.root.join("catalog.enc")
    }

    fn migrations_path(&self) -> PathBuf {
        self.root.join("migrations.enc")
    }

    fn index_catalog_path(&self) -> PathBuf {
        self.root.join("indexes.enc")
    }

    fn index_path(&self, index_name: &str) -> PathBuf {
        self.root.join(format!("index_{index_name}.enc"))
    }
}

fn copy_if_exists(src: &Path, dest: &Path) -> Result<()> {
    if src.exists() {
        fs::copy(src, dest)?;
    }
    Ok(())
}

fn load_encrypted_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    crypto: &CryptoManager,
    default: T,
) -> Result<T> {
    if !path.exists() {
        return Ok(default);
    }
    let encrypted = fs::read_to_string(path)?;
    if encrypted.trim().is_empty() {
        return Ok(default);
    }
    let decrypted = crypto.decrypt_from_base64(&encrypted)?;
    Ok(serde_json::from_slice::<T>(&decrypted)?)
}

fn save_encrypted_json<T: serde::Serialize>(
    path: &Path,
    crypto: &CryptoManager,
    value: &T,
) -> Result<()> {
    let serialized = serde_json::to_vec_pretty(value)?;
    let encrypted = crypto.encrypt_to_base64(&serialized)?;
    atomic_write(path, encrypted.as_bytes())
}

fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = path.with_extension("tmp_write");

    {
        let mut tmp = File::create(&tmp_path)?;
        tmp.write_all(data)?;
        tmp.sync_all()?;
    }
    fs::rename(&tmp_path, path)?;

    if let Ok(dir_handle) = File::open(dir) {
        let _ = dir_handle.sync_all();
    }
    Ok(())
}

fn index_value_key(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}
