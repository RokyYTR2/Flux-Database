use crate::ast::{
    AggregateFunc, AggregateTarget, CompareOp, FilterExpr, JoinClause, OrderByClause, SelectExpr,
    SortOrder, Statement,
};
use crate::error::{FluxError, Result};
use crate::parser::parse_script;
use crate::security::{AuditLogger, CryptoManager, Identity, statement_action};
use crate::storage::Storage;
use crate::types::{ColumnSchema, Constraint, DataType, Row, TableSchema, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone)]
pub enum QueryResult {
    Message(String),
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
}

pub struct Engine {
    storage: Storage,
    identity: Identity,
    audit_logger: AuditLogger,
}

impl Engine {
    pub fn open(
        data_dir: impl AsRef<Path>,
        crypto: CryptoManager,
        identity: Identity,
        audit_logger: AuditLogger,
    ) -> Result<Self> {
        Ok(Self {
            storage: Storage::open(data_dir, crypto)?,
            identity,
            audit_logger,
        })
    }

    pub fn execute_script(&mut self, script: &str) -> Result<Vec<QueryResult>> {
        parse_script(script)?
            .into_iter()
            .map(|statement| self.execute_statement(statement))
            .collect()
    }

    pub fn execute_statement(&mut self, statement: Statement) -> Result<QueryResult> {
        let action = statement_action(&statement);
        if !self.identity.role.allows(&statement) {
            let error = FluxError::AuthorizationDenied {
                user: self.identity.username.clone(),
                action: action.to_string(),
            };
            self.audit_logger
                .log(&self.identity, action, false, Some(&error.to_string()))?;
            return Err(error);
        }

        let result = self.execute_statement_unchecked(statement);
        match &result {
            Ok(_) => {
                self.audit_logger.log(&self.identity, action, true, None)?;
            }
            Err(err) => {
                self.audit_logger
                    .log(&self.identity, action, false, Some(&err.to_string()))?;
            }
        }
        result
    }

    fn execute_statement_unchecked(&mut self, statement: Statement) -> Result<QueryResult> {
        match statement {
            Statement::CreateTable { name, columns } => self.create_table(name, columns),
            Statement::DropTable { name } => self.drop_table(name),
            Statement::CreateIndex {
                name,
                table,
                column,
            } => self.create_index(name, table, column),
            Statement::DropIndex { name } => self.drop_index(name),
            Statement::AlterTableAddColumn {
                table,
                column,
                default,
            } => self.alter_table_add_column(table, column, default),
            Statement::AlterTableDropColumn { table, column } => {
                self.alter_table_drop_column(table, column)
            }
            Statement::AlterTableRenameColumn {
                table,
                old_name,
                new_name,
            } => self.alter_table_rename_column(table, old_name, new_name),
            Statement::Insert {
                table,
                columns,
                values,
            } => self.insert(table, columns, values),
            Statement::Update {
                table,
                assignments,
                filter,
            } => self.update(table, assignments, filter),
            Statement::Select {
                table,
                columns,
                join,
                filter,
                order_by,
                limit,
                offset,
            } => self.select(table, columns, join, filter, order_by, limit, offset),
            Statement::Delete { table, filter } => self.delete(table, filter),
            Statement::Begin => self.begin_transaction(),
            Statement::Commit => self.commit_transaction(),
            Statement::Rollback => self.rollback_transaction(),
            Statement::ShowTables => self.show_tables(),
            Statement::ShowMigrations => self.show_migrations(),
            Statement::Describe { table } => self.describe(table),
        }
    }

    fn begin_transaction(&mut self) -> Result<QueryResult> {
        self.storage.begin_transaction()?;
        Ok(QueryResult::Message("transaction started".to_string()))
    }

    fn commit_transaction(&mut self) -> Result<QueryResult> {
        self.storage.commit_transaction()?;
        Ok(QueryResult::Message("transaction committed".to_string()))
    }

    fn rollback_transaction(&mut self) -> Result<QueryResult> {
        self.storage.rollback_transaction()?;
        Ok(QueryResult::Message("transaction rolled back".to_string()))
    }

    fn create_table(
        &mut self,
        table_name: String,
        columns: Vec<crate::ast::ColumnDef>,
    ) -> Result<QueryResult> {
        if columns.is_empty() {
            return Err(FluxError::InvalidSchema(
                "table must contain at least one column".to_string(),
            ));
        }

        let mut seen = HashSet::new();
        let mut pk_count = 0usize;
        let mut schema_columns = Vec::with_capacity(columns.len());
        for col in columns {
            if !seen.insert(col.name.clone()) {
                return Err(FluxError::InvalidSchema(format!(
                    "duplicate column '{}'",
                    col.name
                )));
            }
            let constraints: Vec<Constraint> = col
                .constraints
                .iter()
                .map(|c| match c {
                    crate::ast::ColumnConstraint::PrimaryKey => Constraint::PrimaryKey,
                    crate::ast::ColumnConstraint::NotNull => Constraint::NotNull,
                    crate::ast::ColumnConstraint::Unique => Constraint::Unique,
                })
                .collect();
            if constraints.contains(&Constraint::PrimaryKey) {
                pk_count += 1;
                if pk_count > 1 {
                    return Err(FluxError::InvalidSchema(
                        "table can have at most one PRIMARY KEY column".to_string(),
                    ));
                }
            }
            schema_columns.push(ColumnSchema {
                name: col.name,
                data_type: col.data_type,
                constraints,
            });
        }

        self.storage.create_table(TableSchema {
            name: table_name.clone(),
            columns: schema_columns,
            next_row_id: 0,
        })?;

        Ok(QueryResult::Message(format!(
            "table '{table_name}' created"
        )))
    }

    fn drop_table(&mut self, name: String) -> Result<QueryResult> {
        self.storage.drop_table(&name)?;
        Ok(QueryResult::Message(format!("table '{name}' dropped")))
    }

    fn create_index(
        &mut self,
        index_name: String,
        table: String,
        column: String,
    ) -> Result<QueryResult> {
        let key_count = self.storage.create_index(&index_name, &table, &column)?;
        Ok(QueryResult::Message(format!(
            "index '{index_name}' created on {table}({column}) with {key_count} key(s)"
        )))
    }

    fn drop_index(&mut self, name: String) -> Result<QueryResult> {
        self.storage.drop_index(&name)?;
        Ok(QueryResult::Message(format!("index '{name}' dropped")))
    }

    fn alter_table_add_column(
        &mut self,
        table_name: String,
        column: crate::ast::ColumnDef,
        default: Option<Value>,
    ) -> Result<QueryResult> {
        let default_value = default.unwrap_or(Value::Null);
        let coerced_default = coerce_value(&column.name, &column.data_type, default_value)?;
        let constraints: Vec<Constraint> = column
            .constraints
            .iter()
            .map(|c| match c {
                crate::ast::ColumnConstraint::PrimaryKey => Constraint::PrimaryKey,
                crate::ast::ColumnConstraint::NotNull => Constraint::NotNull,
                crate::ast::ColumnConstraint::Unique => Constraint::Unique,
            })
            .collect();
        let updated_rows = self.storage.add_column(
            &table_name,
            ColumnSchema {
                name: column.name.clone(),
                data_type: column.data_type.clone(),
                constraints,
            },
            coerced_default,
        )?;
        Ok(QueryResult::Message(format!(
            "table '{table_name}' altered: added column '{}' ({updated_rows} rows backfilled)",
            column.name
        )))
    }

    fn alter_table_drop_column(
        &mut self,
        table: String,
        column: String,
    ) -> Result<QueryResult> {
        let count = self.storage.drop_column(&table, &column)?;
        Ok(QueryResult::Message(format!(
            "table '{table}' altered: dropped column '{column}' ({count} rows updated)"
        )))
    }

    fn alter_table_rename_column(
        &mut self,
        table: String,
        old_name: String,
        new_name: String,
    ) -> Result<QueryResult> {
        let count = self.storage.rename_column(&table, &old_name, &new_name)?;
        Ok(QueryResult::Message(format!(
            "table '{table}' altered: renamed '{old_name}' to '{new_name}' ({count} rows updated)"
        )))
    }

    fn insert(
        &mut self,
        table: String,
        columns: Option<Vec<String>>,
        values: Vec<Value>,
    ) -> Result<QueryResult> {
        let schema = self.storage.get_schema(&table)?;

        let mut row = Row::new();
        if let Some(ref col_names) = columns {
            if values.len() != col_names.len() {
                return Err(FluxError::ValueCountMismatch {
                    expected: col_names.len(),
                    actual: values.len(),
                });
            }
            for name in col_names {
                if !schema.columns.iter().any(|c| c.name == *name) {
                    return Err(FluxError::ColumnNotFound {
                        table: table.clone(),
                        column: name.clone(),
                    });
                }
            }
            for (name, value) in col_names.iter().zip(values.into_iter()) {
                let col_schema = schema.columns.iter().find(|c| c.name == *name).unwrap();
                let casted = coerce_value(&col_schema.name, &col_schema.data_type, value)?;
                row.insert(name.clone(), casted);
            }
            for col in &schema.columns {
                if !row.contains_key(&col.name) {
                    row.insert(col.name.clone(), Value::Null);
                }
            }
        } else {
            if values.len() != schema.columns.len() {
                return Err(FluxError::ValueCountMismatch {
                    expected: schema.columns.len(),
                    actual: values.len(),
                });
            }
            for (column, value) in schema.columns.iter().zip(values.into_iter()) {
                let casted = coerce_value(&column.name, &column.data_type, value)?;
                row.insert(column.name.clone(), casted);
            }
        }

        check_constraints_for_insert(&self.storage, &table, &schema, &row)?;

        self.storage.append_row(&table, &row)?;
        Ok(QueryResult::Message("1 row inserted".to_string()))
    }

    fn update(
        &mut self,
        table: String,
        assignments: Vec<(String, Value)>,
        filter: Option<FilterExpr>,
    ) -> Result<QueryResult> {
        let schema = self.storage.get_schema(&table)?;
        let prepared_filter = match filter {
            Some(filter) => Some(prepare_filter(&schema, &table, filter)?),
            None => None,
        };
        let prepared_assignments = prepare_assignments(&schema, &table, assignments)?;

        check_unique_constraints_for_update(
            &self.storage,
            &table,
            &schema,
            &prepared_assignments,
            prepared_filter.as_ref(),
        )?;

        let mut updated = 0usize;

        self.storage.rewrite_rows(&table, |mut row| {
            if !row_matches_filter(&row, prepared_filter.as_ref()) {
                return Ok(Some(row));
            }
            for (column, value) in &prepared_assignments {
                row.insert(column.clone(), value.clone());
            }
            for (col_name, value) in &prepared_assignments {
                if matches!(value, Value::Null) {
                    if let Some(col_schema) = schema.columns.iter().find(|c| c.name == *col_name) {
                        if col_schema.constraints.contains(&Constraint::NotNull)
                            || col_schema.constraints.contains(&Constraint::PrimaryKey)
                        {
                            return Err(FluxError::ConstraintViolation(format!(
                                "column '{col_name}' cannot be NULL"
                            )));
                        }
                    }
                }
            }
            updated += 1;
            Ok(Some(row))
        })?;

        Ok(QueryResult::Message(format!("{updated} rows updated")))
    }

    fn select(
        &self,
        table: String,
        columns: Vec<SelectExpr>,
        join: Option<JoinClause>,
        filter: Option<FilterExpr>,
        order_by: Vec<OrderByClause>,
        limit: Option<usize>,
        offset: Option<usize>,
    ) -> Result<QueryResult> {
        let schema = self.storage.get_schema(&table)?;

        let has_aggregates = columns.iter().any(|c| matches!(c, SelectExpr::Aggregate { .. }));

        let (rows, effective_schema) = if let Some(ref join_clause) = join {
            let right_schema = self.storage.get_schema(&join_clause.table)?;
            let left_rows = self.storage.read_rows(&table)?;
            let right_rows = self.storage.read_rows(&join_clause.table)?;

            let mut merged_columns: Vec<ColumnSchema> = schema.columns.clone();
            for rc in &right_schema.columns {
                let name = if merged_columns.iter().any(|c| c.name == rc.name) {
                    format!("{}_{}", join_clause.table, rc.name)
                } else {
                    rc.name.clone()
                };
                merged_columns.push(ColumnSchema {
                    name,
                    data_type: rc.data_type.clone(),
                    constraints: rc.constraints.clone(),
                });
            }

            let mut right_buckets: std::collections::HashMap<String, Vec<&Row>> =
                std::collections::HashMap::new();
            for rr in &right_rows {
                let key = right_join_key(rr.get(&join_clause.right_column))?;
                right_buckets.entry(key).or_default().push(rr);
            }

            let mut joined = Vec::new();
            for lr in &left_rows {
                let left_key = right_join_key(lr.get(&join_clause.left_column))?;
                let Some(matches) = right_buckets.get(&left_key) else {
                    continue;
                };
                for rr in matches {
                    {
                        let mut merged = lr.clone();
                        for rc in &right_schema.columns {
                            let key = if lr.contains_key(&rc.name) {
                                format!("{}_{}", join_clause.table, rc.name)
                            } else {
                                rc.name.clone()
                            };
                            merged.insert(
                                key,
                                rr.get(&rc.name).cloned().unwrap_or(Value::Null),
                            );
                        }
                        joined.push(merged);
                    }
                }
            }

            let eff = TableSchema {
                name: table.clone(),
                columns: merged_columns,
                next_row_id: 0,
            };
            (joined, eff)
        } else {
            let prepared_filter = match &filter {
                Some(f) => Some(prepare_filter(&schema, &table, f.clone())?),
                None => None,
            };
            let candidate_row_ids =
                find_best_index_candidate(&self.storage, &table, prepared_filter.as_ref())?;
            let rows = self
                .storage
                .read_rows_filtered(&table, candidate_row_ids.as_deref())?;
            (rows, schema.clone())
        };

        let prepared_filter = match filter {
            Some(f) => Some(prepare_filter(&effective_schema, &table, f)?),
            None => None,
        };
        let filtered: Vec<Row> = rows
            .into_iter()
            .filter(|row| row_matches_filter(row, prepared_filter.as_ref()))
            .collect();

        if has_aggregates {
            return self.execute_aggregate(&columns, &effective_schema, &table, &filtered);
        }

        let projected_columns = resolve_select_columns(&effective_schema, &table, &columns)?;

        let mut result_rows: Vec<Vec<Value>> = filtered
            .iter()
            .map(|row| {
                projected_columns
                    .iter()
                    .map(|col| row.get(col).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();

        if !order_by.is_empty() {
            let order_indices: Vec<(usize, bool)> = order_by
                .iter()
                .filter_map(|ob| {
                    projected_columns.iter().position(|c| *c == ob.column).map(
                        |idx| {
                            (
                                idx,
                                matches!(ob.order, SortOrder::Asc),
                            )
                        },
                    )
                })
                .collect();

            result_rows.sort_by(|a, b| {
                for &(idx, asc) in &order_indices {
                    let ord = compare_ordering(&a[idx], &b[idx]).unwrap_or(std::cmp::Ordering::Equal);
                    let ord = if asc { ord } else { ord.reverse() };
                    if ord != std::cmp::Ordering::Equal {
                        return ord;
                    }
                }
                std::cmp::Ordering::Equal
            });
        }

        let start = offset.unwrap_or(0);
        let result_rows: Vec<Vec<Value>> = result_rows
            .into_iter()
            .skip(start)
            .take(limit.unwrap_or(usize::MAX))
            .collect();

        Ok(QueryResult::Rows {
            columns: projected_columns,
            rows: result_rows,
        })
    }

    fn execute_aggregate(
        &self,
        select_exprs: &[SelectExpr],
        schema: &TableSchema,
        table: &str,
        rows: &[Row],
    ) -> Result<QueryResult> {
        let mut col_names = Vec::new();
        let mut result_values = Vec::new();

        for expr in select_exprs {
            match expr {
                SelectExpr::Aggregate { func, target } => {
                    let col_label = match (func, target) {
                        (f, AggregateTarget::Star) => format!("{}(*)", agg_name(f)),
                        (f, AggregateTarget::Column(c)) => format!("{}({})", agg_name(f), c),
                    };

                    if let AggregateTarget::Column(col) = target {
                        if !schema.columns.iter().any(|c| c.name == *col) {
                            return Err(FluxError::ColumnNotFound {
                                table: table.to_string(),
                                column: col.clone(),
                            });
                        }
                    }

                    let value = compute_aggregate(func, target, rows)?;
                    col_names.push(col_label);
                    result_values.push(value);
                }
                SelectExpr::Column(col) => {
                    if !schema.columns.iter().any(|c| c.name == *col) {
                        return Err(FluxError::ColumnNotFound {
                            table: table.to_string(),
                            column: col.clone(),
                        });
                    }
                    let value = rows
                        .first()
                        .and_then(|r| r.get(col).cloned())
                        .unwrap_or(Value::Null);
                    col_names.push(col.clone());
                    result_values.push(value);
                }
                SelectExpr::AllColumns => {
                    return Err(FluxError::Parse(
                        "cannot mix * with aggregate functions".to_string(),
                    ));
                }
            }
        }

        Ok(QueryResult::Rows {
            columns: col_names,
            rows: vec![result_values],
        })
    }

    fn delete(&mut self, table: String, filter: Option<FilterExpr>) -> Result<QueryResult> {
        let schema = self.storage.get_schema(&table)?;
        let prepared_filter = match filter {
            Some(filter) => Some(prepare_filter(&schema, &table, filter)?),
            None => None,
        };

        let mut deleted = 0usize;
        self.storage.rewrite_rows(&table, |row| {
            if row_matches_filter(&row, prepared_filter.as_ref()) {
                deleted += 1;
                Ok(None)
            } else {
                Ok(Some(row))
            }
        })?;
        Ok(QueryResult::Message(format!("{deleted} rows deleted")))
    }

    fn show_tables(&self) -> Result<QueryResult> {
        let rows = self
            .storage
            .list_tables()
            .into_iter()
            .map(|table| vec![Value::Text(table)])
            .collect::<Vec<_>>();

        Ok(QueryResult::Rows {
            columns: vec!["table_name".to_string()],
            rows,
        })
    }

    fn show_migrations(&self) -> Result<QueryResult> {
        let rows = self
            .storage
            .list_migrations()
            .into_iter()
            .map(|migration| {
                vec![
                    Value::Text(migration.id.to_string()),
                    Value::Text(migration.executed_at_unix_ms.to_string()),
                    Value::Text(migration.table_name),
                    Value::Text(migration.operation),
                    Value::Text(migration.details),
                ]
            })
            .collect::<Vec<_>>();

        Ok(QueryResult::Rows {
            columns: vec![
                "id".to_string(),
                "executed_at_unix_ms".to_string(),
                "table_name".to_string(),
                "operation".to_string(),
                "details".to_string(),
            ],
            rows,
        })
    }

    fn describe(&self, table: String) -> Result<QueryResult> {
        let schema = self.storage.get_schema(&table)?;
        let rows = schema
            .columns
            .into_iter()
            .map(|column| {
                let constraints_str = column
                    .constraints
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                vec![
                    Value::Text(column.name),
                    Value::Text(column.data_type.to_string()),
                    Value::Text(constraints_str),
                ]
            })
            .collect::<Vec<_>>();

        Ok(QueryResult::Rows {
            columns: vec![
                "column_name".to_string(),
                "data_type".to_string(),
                "constraints".to_string(),
            ],
            rows,
        })
    }
}


fn check_constraints_for_insert(
    storage: &Storage,
    table: &str,
    schema: &TableSchema,
    row: &Row,
) -> Result<()> {
    for col_schema in &schema.columns {
        let value = row.get(&col_schema.name).unwrap_or(&Value::Null);

        if matches!(value, Value::Null) {
            if col_schema.constraints.contains(&Constraint::NotNull)
                || col_schema.constraints.contains(&Constraint::PrimaryKey)
            {
                return Err(FluxError::ConstraintViolation(format!(
                    "column '{}' cannot be NULL",
                    col_schema.name
                )));
            }
        }

        if col_schema.constraints.contains(&Constraint::Unique)
            || col_schema.constraints.contains(&Constraint::PrimaryKey)
        {
            if !matches!(value, Value::Null) {
                let existing = storage.read_rows(table)?;
                for existing_row in &existing {
                    if let Some(existing_val) = existing_row.get(&col_schema.name) {
                        if existing_val == value {
                            return Err(FluxError::ConstraintViolation(format!(
                                "duplicate value for column '{}': {}",
                                col_schema.name, value
                            )));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}


fn check_unique_constraints_for_update(
    storage: &Storage,
    table: &str,
    schema: &TableSchema,
    assignments: &[(String, Value)],
    filter: Option<&FilterExpr>,
) -> Result<()> {
    let unique_cols: Vec<&str> = schema
        .columns
        .iter()
        .filter(|c| {
            (c.constraints.contains(&Constraint::Unique)
                || c.constraints.contains(&Constraint::PrimaryKey))
                && assignments.iter().any(|(name, _)| name == &c.name)
        })
        .map(|c| c.name.as_str())
        .collect();
    if unique_cols.is_empty() {
        return Ok(());
    }

    let rows = storage.read_rows(table)?;
    for col in unique_cols {
        let mut seen = std::collections::HashSet::new();
        for row in &rows {
            let value = if row_matches_filter(row, filter) {
                assignments
                    .iter()
                    .find(|(name, _)| name == col)
                    .map(|(_, v)| v)
                    .or_else(|| row.get(col))
            } else {
                row.get(col)
            };
            let Some(value) = value else { continue };
            if matches!(value, Value::Null) {
                continue;
            }
            let key = index_value_key_for(value)?;
            if !seen.insert(key) {
                return Err(FluxError::ConstraintViolation(format!(
                    "duplicate value for column '{col}': {value}"
                )));
            }
        }
    }
    Ok(())
}

fn index_value_key_for(value: &Value) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn right_join_key(value: Option<&Value>) -> Result<String> {
    index_value_key_for(value.unwrap_or(&Value::Null))
}

fn agg_name(func: &AggregateFunc) -> &'static str {
    match func {
        AggregateFunc::Count => "COUNT",
        AggregateFunc::Sum => "SUM",
        AggregateFunc::Min => "MIN",
        AggregateFunc::Max => "MAX",
        AggregateFunc::Avg => "AVG",
    }
}

fn compute_aggregate(func: &AggregateFunc, target: &AggregateTarget, rows: &[Row]) -> Result<Value> {
    match func {
        AggregateFunc::Count => {
            let count = match target {
                AggregateTarget::Star => rows.len(),
                AggregateTarget::Column(col) => rows
                    .iter()
                    .filter(|r| !matches!(r.get(col.as_str()), None | Some(Value::Null)))
                    .count(),
            };
            Ok(Value::Int(count as i64))
        }
        AggregateFunc::Sum => {
            let col = agg_column(target)?;
            let mut sum: i64 = 0;
            for row in rows {
                match row.get(col) {
                    Some(Value::Int(v)) => sum += v,
                    Some(Value::Null) | None => {}
                    _ => {
                        return Err(FluxError::TypeMismatch {
                            column: col.to_string(),
                            expected: DataType::Int,
                            found: row.get(col).and_then(|v| v.data_type()),
                        });
                    }
                }
            }
            Ok(Value::Int(sum))
        }
        AggregateFunc::Avg => {
            let col = agg_column(target)?;
            let mut sum: i64 = 0;
            let mut count: i64 = 0;
            for row in rows {
                match row.get(col) {
                    Some(Value::Int(v)) => {
                        sum += v;
                        count += 1;
                    }
                    Some(Value::Null) | None => {}
                    _ => {
                        return Err(FluxError::TypeMismatch {
                            column: col.to_string(),
                            expected: DataType::Int,
                            found: row.get(col).and_then(|v| v.data_type()),
                        });
                    }
                }
            }
            if count == 0 {
                Ok(Value::Null)
            } else {
                Ok(Value::Int(sum / count))
            }
        }
        AggregateFunc::Min => {
            let col = agg_column(target)?;
            let mut min: Option<&Value> = None;
            for row in rows {
                match row.get(col) {
                    Some(Value::Null) | None => {}
                    Some(v) => {
                        if min.is_none()
                            || compare_ordering(v, min.unwrap())
                                .is_some_and(|o| o.is_lt())
                        {
                            min = Some(v);
                        }
                    }
                }
            }
            Ok(min.cloned().unwrap_or(Value::Null))
        }
        AggregateFunc::Max => {
            let col = agg_column(target)?;
            let mut max: Option<&Value> = None;
            for row in rows {
                match row.get(col) {
                    Some(Value::Null) | None => {}
                    Some(v) => {
                        if max.is_none()
                            || compare_ordering(v, max.unwrap())
                                .is_some_and(|o| o.is_gt())
                        {
                            max = Some(v);
                        }
                    }
                }
            }
            Ok(max.cloned().unwrap_or(Value::Null))
        }
    }
}

fn agg_column<'a>(target: &'a AggregateTarget) -> Result<&'a str> {
    match target {
        AggregateTarget::Column(c) => Ok(c.as_str()),
        AggregateTarget::Star => Err(FluxError::Parse(
            "this aggregate function requires a column name, not *".to_string(),
        )),
    }
}


fn resolve_select_columns(
    schema: &TableSchema,
    table: &str,
    exprs: &[SelectExpr],
) -> Result<Vec<String>> {
    let mut result = Vec::new();
    for expr in exprs {
        match expr {
            SelectExpr::AllColumns => {
                for col in &schema.columns {
                    result.push(col.name.clone());
                }
            }
            SelectExpr::Column(name) => {
                if !schema.columns.iter().any(|c| c.name == *name) {
                    return Err(FluxError::ColumnNotFound {
                        table: table.to_string(),
                        column: name.clone(),
                    });
                }
                result.push(name.clone());
            }
            SelectExpr::Aggregate { .. } => {
            }
        }
    }
    Ok(result)
}

fn prepare_assignments(
    schema: &TableSchema,
    table: &str,
    assignments: Vec<(String, Value)>,
) -> Result<Vec<(String, Value)>> {
    if assignments.is_empty() {
        return Err(FluxError::Parse(
            "UPDATE requires at least one assignment".to_string(),
        ));
    }

    let mut seen = HashSet::new();
    let mut prepared = Vec::with_capacity(assignments.len());
    let mut column_types = BTreeMap::new();
    for column in &schema.columns {
        column_types.insert(column.name.clone(), column.data_type.clone());
    }

    for (column, value) in assignments {
        if !seen.insert(column.clone()) {
            return Err(FluxError::InvalidSchema(format!(
                "column '{column}' assigned multiple times in UPDATE"
            )));
        }
        let expected_type =
            column_types
                .get(&column)
                .cloned()
                .ok_or_else(|| FluxError::ColumnNotFound {
                    table: table.to_string(),
                    column: column.clone(),
                })?;
        prepared.push((
            column.clone(),
            coerce_value(&column, &expected_type, value)?,
        ));
    }

    Ok(prepared)
}

fn prepare_filter(schema: &TableSchema, table: &str, filter: FilterExpr) -> Result<FilterExpr> {
    match filter {
        FilterExpr::Compare { column, op, value } => {
            let expected_type = schema
                .columns
                .iter()
                .find(|candidate| candidate.name == column)
                .map(|candidate| candidate.data_type.clone())
                .ok_or_else(|| FluxError::ColumnNotFound {
                    table: table.to_string(),
                    column: column.clone(),
                })?;

            if matches!(op, CompareOp::Like) {
                if expected_type != DataType::Text {
                    return Err(FluxError::TypeMismatch {
                        column: column.clone(),
                        expected: DataType::Text,
                        found: Some(expected_type),
                    });
                }
                let value = match value {
                    Value::Text(v) => Value::Text(v),
                    other => {
                        return Err(FluxError::TypeMismatch {
                            column,
                            expected: DataType::Text,
                            found: other.data_type(),
                        });
                    }
                };
                return Ok(FilterExpr::Compare { column, op, value });
            }

            let value = coerce_value(&column, &expected_type, value)?;
            Ok(FilterExpr::Compare { column, op, value })
        }
        FilterExpr::IsNull { column } => {
            if !schema.columns.iter().any(|c| c.name == column) {
                return Err(FluxError::ColumnNotFound {
                    table: table.to_string(),
                    column,
                });
            }
            Ok(FilterExpr::IsNull { column })
        }
        FilterExpr::IsNotNull { column } => {
            if !schema.columns.iter().any(|c| c.name == column) {
                return Err(FluxError::ColumnNotFound {
                    table: table.to_string(),
                    column,
                });
            }
            Ok(FilterExpr::IsNotNull { column })
        }
        FilterExpr::And(left, right) => Ok(FilterExpr::And(
            Box::new(prepare_filter(schema, table, *left)?),
            Box::new(prepare_filter(schema, table, *right)?),
        )),
        FilterExpr::Or(left, right) => Ok(FilterExpr::Or(
            Box::new(prepare_filter(schema, table, *left)?),
            Box::new(prepare_filter(schema, table, *right)?),
        )),
    }
}

fn row_matches_filter(row: &Row, filter: Option<&FilterExpr>) -> bool {
    let Some(filter) = filter else {
        return true;
    };
    evaluate_filter(row, filter)
}

fn evaluate_filter(row: &Row, filter: &FilterExpr) -> bool {
    match filter {
        FilterExpr::Compare { column, op, value } => {
            let left = row.get(column).unwrap_or(&Value::Null);
            compare_values(left, op, value)
        }
        FilterExpr::IsNull { column } => {
            matches!(row.get(column), None | Some(Value::Null))
        }
        FilterExpr::IsNotNull { column } => {
            matches!(row.get(column), Some(v) if !matches!(v, Value::Null))
        }
        FilterExpr::And(left, right) => evaluate_filter(row, left) && evaluate_filter(row, right),
        FilterExpr::Or(left, right) => evaluate_filter(row, left) || evaluate_filter(row, right),
    }
}

fn compare_values(left: &Value, op: &CompareOp, right: &Value) -> bool {
    match op {
        CompareOp::Eq => left == right,
        CompareOp::NotEq => left != right,
        CompareOp::Gt => compare_ordering(left, right).is_some_and(|ord| ord.is_gt()),
        CompareOp::Gte => compare_ordering(left, right).is_some_and(|ord| ord.is_ge()),
        CompareOp::Lt => compare_ordering(left, right).is_some_and(|ord| ord.is_lt()),
        CompareOp::Lte => compare_ordering(left, right).is_some_and(|ord| ord.is_le()),
        CompareOp::Like => match (left, right) {
            (Value::Text(input), Value::Text(pattern)) => like_matches(input, pattern),
            _ => false,
        },
    }
}

fn compare_ordering(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
        (Value::Text(a), Value::Text(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        _ => None,
    }
}

fn like_matches(input: &str, pattern: &str) -> bool {
    let input_chars = input.chars().collect::<Vec<_>>();
    let pattern_chars = pattern.chars().collect::<Vec<_>>();
    let mut dp = vec![vec![false; input_chars.len() + 1]; pattern_chars.len() + 1];
    dp[0][0] = true;

    for i in 1..=pattern_chars.len() {
        if pattern_chars[i - 1] == '%' {
            dp[i][0] = dp[i - 1][0];
        }
    }

    for i in 1..=pattern_chars.len() {
        for j in 1..=input_chars.len() {
            let p = pattern_chars[i - 1];
            let c = input_chars[j - 1];
            dp[i][j] = match p {
                '%' => dp[i - 1][j] || dp[i][j - 1],
                '_' => dp[i - 1][j - 1],
                _ => dp[i - 1][j - 1] && p == c,
            };
        }
    }

    dp[pattern_chars.len()][input_chars.len()]
}

fn find_best_index_candidate(
    storage: &Storage,
    table: &str,
    filter: Option<&FilterExpr>,
) -> Result<Option<Vec<u64>>> {
    let Some(filter) = filter else {
        return Ok(None);
    };
    let mut candidates = Vec::new();
    collect_index_candidates(filter, &mut candidates);
    let mut best: Option<Vec<u64>> = None;
    for (column, value) in candidates {
        if let Some(ids) = storage.find_indexed_row_ids(table, &column, &value)? {
            if best
                .as_ref()
                .is_none_or(|existing| ids.len() < existing.len())
            {
                best = Some(ids);
            }
        }
    }
    Ok(best)
}

fn collect_index_candidates(filter: &FilterExpr, out: &mut Vec<(String, Value)>) {
    match filter {
        FilterExpr::Compare { column, op, value } => {
            if matches!(op, CompareOp::Eq) {
                out.push((column.clone(), value.clone()));
            }
        }
        FilterExpr::And(left, right) => {
            collect_index_candidates(left, out);
            collect_index_candidates(right, out);
        }
        FilterExpr::Or(_, _) | FilterExpr::IsNull { .. } | FilterExpr::IsNotNull { .. } => {}
    }
}

fn coerce_value(column_name: &str, expected: &DataType, value: Value) -> Result<Value> {
    let found = value.data_type();
    match (expected, value) {
        (_, Value::Null) => Ok(Value::Null),
        (DataType::Int, Value::Int(v)) => Ok(Value::Int(v)),
        (DataType::Int, Value::Text(v)) => {
            v.parse::<i64>()
                .map(Value::Int)
                .map_err(|_| FluxError::TypeMismatch {
                    column: column_name.to_string(),
                    expected: expected.clone(),
                    found,
                })
        }
        (DataType::Bool, Value::Bool(v)) => Ok(Value::Bool(v)),
        (DataType::Bool, Value::Text(v)) => match v.to_ascii_lowercase().as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(FluxError::TypeMismatch {
                column: column_name.to_string(),
                expected: expected.clone(),
                found,
            }),
        },
        (DataType::Text, Value::Text(v)) => Ok(Value::Text(v)),
        (DataType::Text, Value::Int(v)) => Ok(Value::Text(v.to_string())),
        (DataType::Text, Value::Bool(v)) => Ok(Value::Text(v.to_string())),
        _ => Err(FluxError::TypeMismatch {
            column: column_name.to_string(),
            expected: expected.clone(),
            found,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::Role;
    use tempfile::tempdir;

    fn test_engine() -> (Engine, tempfile::TempDir) {
        let temp = tempdir().expect("tempdir should be created");
        let key = CryptoManager::generate_base64_key();
        let crypto = CryptoManager::from_base64_key(&key).expect("key parse");
        let audit_logger = AuditLogger::open(temp.path()).expect("audit open");
        let identity = Identity {
            username: "test_admin".to_string(),
            role: Role::Admin,
        };
        (
            Engine::open(temp.path(), crypto, identity, audit_logger).expect("engine should start"),
            temp,
        )
    }

    #[test]
    fn create_insert_select_delete_flow() {
        let (mut engine, _tmp) = test_engine();

        engine
            .execute_script("CREATE TABLE users (id INT, name TEXT, active BOOL);")
            .expect("table creation should succeed");
        engine
            .execute_script("INSERT INTO users VALUES (1, 'Ana', true);")
            .expect("first insert should succeed");
        engine
            .execute_script("INSERT INTO users VALUES (2, 'Bob', false);")
            .expect("second insert should succeed");

        let selected = engine
            .execute_script("SELECT id, name FROM users WHERE active = true;")
            .expect("select should succeed");
        let QueryResult::Rows { columns, rows } = &selected[0] else {
            panic!("expected rows result");
        };
        assert_eq!(columns, &vec!["id".to_string(), "name".to_string()]);
        assert_eq!(
            rows,
            &vec![vec![Value::Int(1), Value::Text("Ana".to_string())]]
        );

        engine
            .execute_script("DELETE FROM users WHERE id = 2;")
            .expect("delete should succeed");
        let selected_after_delete = engine
            .execute_script("SELECT * FROM users;")
            .expect("select after delete should succeed");
        let QueryResult::Rows { rows, .. } = &selected_after_delete[0] else {
            panic!("expected rows result");
        };
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn update_where_and_index_flow() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE users (id INT, name TEXT, active BOOL);
                 INSERT INTO users VALUES (1, 'Ana', true);
                 INSERT INTO users VALUES (2, 'Bob', false);
                 CREATE INDEX idx_users_id ON users(id);",
            )
            .expect("setup should pass");

        engine
            .execute_script("UPDATE users SET active = true, name = 'Bobby' WHERE id = 2;")
            .expect("update should pass");

        let selected = engine
            .execute_script("SELECT id, name FROM users WHERE id = 2 AND active = true;")
            .expect("select should pass");
        let QueryResult::Rows { rows, .. } = &selected[0] else {
            panic!("expected rows");
        };
        assert_eq!(
            rows,
            &vec![vec![Value::Int(2), Value::Text("Bobby".to_string())]]
        );
    }

    #[test]
    fn where_supports_comparison_and_like() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE metrics (id INT, name TEXT, active BOOL);
                 INSERT INTO metrics VALUES (1, 'alpha', true);
                 INSERT INTO metrics VALUES (2, 'beta', false);
                 INSERT INTO metrics VALUES (3, 'alphabet', true);",
            )
            .expect("setup should pass");

        let selected = engine
            .execute_script("SELECT id FROM metrics WHERE id > 1 AND name LIKE 'a%';")
            .expect("select should pass");
        let QueryResult::Rows { rows, .. } = &selected[0] else {
            panic!("expected rows");
        };
        assert_eq!(rows, &vec![vec![Value::Int(3)]]);
    }

    #[test]
    fn drop_table_and_index() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT);
                 CREATE INDEX idx ON t(id);
                 DROP TABLE t;",
            )
            .expect("drop table should pass");

        assert!(engine.execute_script("SELECT * FROM t;").is_err());
    }

    #[test]
    fn drop_index_standalone() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT);
                 CREATE INDEX idx ON t(id);
                 DROP INDEX idx;",
            )
            .expect("drop index should pass");
    }

    #[test]
    fn insert_with_columns() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT, name TEXT, active BOOL);
                 INSERT INTO t (id, name) VALUES (1, 'Alice');",
            )
            .expect("insert with columns");

        let result = engine
            .execute_script("SELECT * FROM t;")
            .expect("select");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][2], Value::Null);
    }

    #[test]
    fn order_by_limit_offset() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT, name TEXT);
                 INSERT INTO t VALUES (3, 'C');
                 INSERT INTO t VALUES (1, 'A');
                 INSERT INTO t VALUES (2, 'B');",
            )
            .expect("setup");

        let result = engine
            .execute_script("SELECT id, name FROM t ORDER BY id ASC LIMIT 2 OFFSET 1;")
            .expect("select");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], Value::Int(2));
        assert_eq!(rows[1][0], Value::Int(3));
    }

    #[test]
    fn aggregate_functions() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT, name TEXT);
                 INSERT INTO t VALUES (10, 'A');
                 INSERT INTO t VALUES (20, 'B');
                 INSERT INTO t VALUES (30, 'C');",
            )
            .expect("setup");

        let result = engine
            .execute_script("SELECT COUNT(*), SUM(id), MIN(id), MAX(id), AVG(id) FROM t;")
            .expect("aggregate");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], Value::Int(3));
        assert_eq!(rows[0][1], Value::Int(60));
        assert_eq!(rows[0][2], Value::Int(10));
        assert_eq!(rows[0][3], Value::Int(30));
        assert_eq!(rows[0][4], Value::Int(20));
    }

    #[test]
    fn primary_key_prevents_duplicates() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
                 INSERT INTO t VALUES (1, 'Alice');",
            )
            .expect("setup");

        let err = engine
            .execute_script("INSERT INTO t VALUES (1, 'Bob');")
            .unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn update_rejects_duplicate_primary_key() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
                 INSERT INTO t VALUES (1, 'Alice');
                 INSERT INTO t VALUES (2, 'Bob');",
            )
            .expect("setup");

        let err = engine
            .execute_script("UPDATE t SET id = 1 WHERE id = 2;")
            .unwrap_err();
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn update_allows_keeping_own_unique_value() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT PRIMARY KEY, name TEXT);
                 INSERT INTO t VALUES (1, 'Alice');",
            )
            .expect("setup");

        engine
            .execute_script("UPDATE t SET name = 'Alicia' WHERE id = 1;")
            .expect("update should pass");
    }

    #[test]
    fn not_null_constraint() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script("CREATE TABLE t (id INT NOT NULL, name TEXT);")
            .expect("create");

        let err = engine
            .execute_script("INSERT INTO t VALUES (NULL, 'Alice');")
            .unwrap_err();
        assert!(err.to_string().contains("NULL"));
    }

    #[test]
    fn alter_table_drop_and_rename_column() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT, name TEXT, extra TEXT);
                 INSERT INTO t VALUES (1, 'Alice', 'x');
                 ALTER TABLE t DROP COLUMN extra;
                 ALTER TABLE t RENAME COLUMN name TO full_name;",
            )
            .expect("alter operations");

        let result = engine.execute_script("SELECT * FROM t;").expect("select");
        let QueryResult::Rows { columns, rows } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(columns, &vec!["id".to_string(), "full_name".to_string()]);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn transaction_rollback() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT);
                 INSERT INTO t VALUES (1);
                 BEGIN;
                 INSERT INTO t VALUES (2);
                 INSERT INTO t VALUES (3);
                 ROLLBACK;",
            )
            .expect("transaction");

        let result = engine
            .execute_script("SELECT COUNT(*) FROM t;")
            .expect("count");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(rows[0][0], Value::Int(1));
    }

    #[test]
    fn transaction_commit() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT);
                 BEGIN;
                 INSERT INTO t VALUES (1);
                 INSERT INTO t VALUES (2);
                 COMMIT;",
            )
            .expect("transaction");

        let result = engine
            .execute_script("SELECT COUNT(*) FROM t;")
            .expect("count");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(rows[0][0], Value::Int(2));
    }

    #[test]
    fn join_matches_only_equal_keys_and_preserves_order() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE users (id INT, name TEXT);
                 CREATE TABLE orders (id INT, user_id INT, product TEXT);
                 INSERT INTO users VALUES (1, 'Alice');
                 INSERT INTO users VALUES (2, 'Bob');
                 INSERT INTO users VALUES (3, 'Carol');
                 INSERT INTO orders VALUES (10, 1, 'Widget');
                 INSERT INTO orders VALUES (11, 2, 'Gadget');
                 INSERT INTO orders VALUES (12, 1, 'Gizmo');",
            )
            .expect("setup");

        let result = engine
            .execute_script(
                "SELECT name, product FROM users JOIN orders ON users.id = orders.user_id;",
            )
            .expect("join");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(
            rows,
            &vec![
                vec![Value::Text("Alice".into()), Value::Text("Widget".into())],
                vec![Value::Text("Alice".into()), Value::Text("Gizmo".into())],
                vec![Value::Text("Bob".into()), Value::Text("Gadget".into())],
            ]
        );
    }

    #[test]
    fn writes_leave_no_temp_files() {
        let (mut engine, tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT, name TEXT);
                 INSERT INTO t VALUES (1, 'A');
                 INSERT INTO t VALUES (2, 'B');
                 CREATE INDEX idx_t_id ON t(id);
                 UPDATE t SET name = 'AA' WHERE id = 1;
                 DELETE FROM t WHERE id = 2;
                 ALTER TABLE t ADD COLUMN active BOOL DEFAULT true;",
            )
            .expect("ops");

        let leftovers: Vec<String> = std::fs::read_dir(tmp.path())
            .expect("read data dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("tmp_write") || name.contains("tmp_rewrite"))
            .collect();
        assert!(leftovers.is_empty(), "leftover temp files: {leftovers:?}");
    }

    #[test]
    fn join_tables() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE users (id INT, name TEXT);
                 CREATE TABLE orders (id INT, user_id INT, product TEXT);
                 INSERT INTO users VALUES (1, 'Alice');
                 INSERT INTO users VALUES (2, 'Bob');
                 INSERT INTO orders VALUES (10, 1, 'Widget');
                 INSERT INTO orders VALUES (11, 2, 'Gadget');
                 INSERT INTO orders VALUES (12, 1, 'Gizmo');",
            )
            .expect("setup");

        let result = engine
            .execute_script(
                "SELECT name, product FROM users JOIN orders ON users.id = orders.user_id WHERE name = 'Alice';",
            )
            .expect("join");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn indexed_query_correct_after_delete() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT, name TEXT);
                 INSERT INTO t VALUES (1, 'A');
                 INSERT INTO t VALUES (2, 'B');
                 INSERT INTO t VALUES (3, 'C');
                 CREATE INDEX idx_t_id ON t(id);
                 DELETE FROM t WHERE id = 2;",
            )
            .expect("setup");

        let result = engine
            .execute_script("SELECT id, name FROM t WHERE id = 3;")
            .expect("select");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(
            rows,
            &vec![vec![Value::Int(3), Value::Text("C".to_string())]]
        );
    }

    #[test]
    fn indexed_query_correct_after_delete_then_insert() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script(
                "CREATE TABLE t (id INT, name TEXT);
                 INSERT INTO t VALUES (1, 'A');
                 INSERT INTO t VALUES (2, 'B');
                 CREATE INDEX idx_t_id ON t(id);
                 DELETE FROM t WHERE id = 1;
                 INSERT INTO t VALUES (3, 'C');",
            )
            .expect("setup");

        for (id, name) in [(2, "B"), (3, "C")] {
            let result = engine
                .execute_script(&format!("SELECT id, name FROM t WHERE id = {id};"))
                .expect("select");
            let QueryResult::Rows { rows, .. } = &result[0] else {
                panic!("expected rows");
            };
            assert_eq!(
                rows,
                &vec![vec![Value::Int(id), Value::Text(name.to_string())]]
            );
        }
    }

    #[test]
    fn indexed_query_correct_after_many_inserts() {
        let (mut engine, _tmp) = test_engine();
        engine
            .execute_script("CREATE TABLE t (id INT, name TEXT); CREATE INDEX idx_t_id ON t(id);")
            .expect("setup");
        for i in 0..20 {
            engine
                .execute_script(&format!("INSERT INTO t VALUES ({i}, 'name{i}');"))
                .expect("insert");
        }

        let result = engine
            .execute_script("SELECT id, name FROM t WHERE id = 17;")
            .expect("select");
        let QueryResult::Rows { rows, .. } = &result[0] else {
            panic!("expected rows");
        };
        assert_eq!(
            rows,
            &vec![vec![Value::Int(17), Value::Text("name17".to_string())]]
        );
    }
}
