use crate::types::{DataType, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ColumnConstraint {
    PrimaryKey,
    NotNull,
    Unique,
}

#[derive(Debug, Clone)]
pub struct ColumnDef {
    pub name: String,
    pub data_type: DataType,
    pub constraints: Vec<ColumnConstraint>,
}

#[derive(Debug, Clone)]
pub enum AggregateFunc {
    Count,
    Sum,
    Min,
    Max,
    Avg,
}

#[derive(Debug, Clone)]
pub enum AggregateTarget {
    Star,
    Column(String),
}

#[derive(Debug, Clone)]
pub enum SelectExpr {
    Column(String),
    AllColumns,
    Aggregate {
        func: AggregateFunc,
        target: AggregateTarget,
    },
}

#[derive(Debug, Clone)]
pub enum CompareOp {
    Eq,
    NotEq,
    Gt,
    Gte,
    Lt,
    Lte,
    Like,
}

#[derive(Debug, Clone)]
pub enum FilterExpr {
    Compare {
        column: String,
        op: CompareOp,
        value: Value,
    },
    IsNull {
        column: String,
    },
    IsNotNull {
        column: String,
    },
    And(Box<FilterExpr>, Box<FilterExpr>),
    Or(Box<FilterExpr>, Box<FilterExpr>),
}

#[derive(Debug, Clone)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub struct OrderByClause {
    pub column: String,
    pub order: SortOrder,
}

#[derive(Debug, Clone)]
pub struct JoinClause {
    pub table: String,
    pub left_column: String,
    pub right_column: String,
}

#[derive(Debug, Clone)]
pub enum Statement {
    CreateTable {
        name: String,
        columns: Vec<ColumnDef>,
    },
    DropTable {
        name: String,
    },
    CreateIndex {
        name: String,
        table: String,
        column: String,
    },
    DropIndex {
        name: String,
    },
    AlterTableAddColumn {
        table: String,
        column: ColumnDef,
        default: Option<Value>,
    },
    AlterTableDropColumn {
        table: String,
        column: String,
    },
    AlterTableRenameColumn {
        table: String,
        old_name: String,
        new_name: String,
    },
    Insert {
        table: String,
        columns: Option<Vec<String>>,
        values: Vec<Value>,
    },
    Update {
        table: String,
        assignments: Vec<(String, Value)>,
        filter: Option<FilterExpr>,
    },
    Select {
        table: String,
        columns: Vec<SelectExpr>,
        join: Option<JoinClause>,
        filter: Option<FilterExpr>,
        order_by: Vec<OrderByClause>,
        limit: Option<usize>,
        offset: Option<usize>,
    },
    Delete {
        table: String,
        filter: Option<FilterExpr>,
    },
    Begin,
    Commit,
    Rollback,
    ShowTables,
    ShowMigrations,
    Describe {
        table: String,
    },
}
