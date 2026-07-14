use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DataType {
    Int,
    Text,
    Bool,
}

impl DataType {
    pub fn parse(input: &str) -> Option<Self> {
        match input.trim().to_ascii_uppercase().as_str() {
            "INT" | "INTEGER" => Some(Self::Int),
            "TEXT" | "STRING" => Some(Self::Text),
            "BOOL" | "BOOLEAN" => Some(Self::Bool),
            _ => None,
        }
    }
}

impl Display for DataType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int => write!(f, "INT"),
            Self::Text => write!(f, "TEXT"),
            Self::Bool => write!(f, "BOOL"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", content = "value")]
pub enum Value {
    Int(i64),
    Text(String),
    Bool(bool),
    Null,
}

impl Value {
    pub fn data_type(&self) -> Option<DataType> {
        match self {
            Self::Int(_) => Some(DataType::Int),
            Self::Text(_) => Some(DataType::Text),
            Self::Bool(_) => Some(DataType::Bool),
            Self::Null => None,
        }
    }
}

impl Display for Value {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Int(v) => write!(f, "{v}"),
            Self::Text(v) => write!(f, "{v}"),
            Self::Bool(v) => write!(f, "{v}"),
            Self::Null => write!(f, "NULL"),
        }
    }
}

pub type Row = BTreeMap<String, Value>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Constraint {
    PrimaryKey,
    NotNull,
    Unique,
}

impl Display for Constraint {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PrimaryKey => write!(f, "PRIMARY KEY"),
            Self::NotNull => write!(f, "NOT NULL"),
            Self::Unique => write!(f, "UNIQUE"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnSchema {
    pub name: String,
    pub data_type: DataType,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnSchema>,
    #[serde(default)]
    pub next_row_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredRow {
    pub id: u64,
    pub data: Row,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub tables: BTreeMap<String, TableSchema>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationRecord {
    pub id: u64,
    pub executed_at_unix_ms: u128,
    pub table_name: String,
    pub operation: String,
    pub details: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexDefinition {
    pub name: String,
    pub table_name: String,
    pub column_name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexCatalog {
    pub indexes: Vec<IndexDefinition>,
}
