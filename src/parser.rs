use crate::ast::{
    AggregateFunc, AggregateTarget, ColumnConstraint, ColumnDef, CompareOp, FilterExpr, JoinClause,
    OrderByClause, SelectExpr, SortOrder, Statement,
};
use crate::error::{FluxError, Result};
use crate::types::{DataType, Value};

pub fn parse_script(input: &str) -> Result<Vec<Statement>> {
    split_statements(input)?
        .into_iter()
        .map(|statement| parse_statement(&statement))
        .collect()
}

pub fn parse_statement(input: &str) -> Result<Statement> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(FluxError::Parse("empty statement".to_string()));
    }

    if is_exact_keywords(trimmed, &["SHOW", "TABLES"]) {
        return Ok(Statement::ShowTables);
    }
    if is_exact_keywords(trimmed, &["SHOW", "MIGRATIONS"]) {
        return Ok(Statement::ShowMigrations);
    }
    if is_exact_keywords(trimmed, &["BEGIN"]) {
        return Ok(Statement::Begin);
    }
    if is_exact_keywords(trimmed, &["COMMIT"]) {
        return Ok(Statement::Commit);
    }
    if is_exact_keywords(trimmed, &["ROLLBACK"]) {
        return Ok(Statement::Rollback);
    }

    if let Some(rest) = strip_prefix_ci(trimmed, "DESCRIBE") {
        let table = normalize_identifier(rest.trim())?;
        return Ok(Statement::Describe { table });
    }

    if strip_prefix_ci(trimmed, "CREATE TABLE").is_some() {
        return parse_create_table(trimmed);
    }
    if strip_prefix_ci(trimmed, "CREATE INDEX").is_some() {
        return parse_create_index(trimmed);
    }
    if strip_prefix_ci(trimmed, "DROP TABLE").is_some() {
        return parse_drop_table(trimmed);
    }
    if strip_prefix_ci(trimmed, "DROP INDEX").is_some() {
        return parse_drop_index(trimmed);
    }
    if strip_prefix_ci(trimmed, "INSERT INTO").is_some() {
        return parse_insert(trimmed);
    }
    if strip_prefix_ci(trimmed, "UPDATE").is_some() {
        return parse_update(trimmed);
    }
    if strip_prefix_ci(trimmed, "ALTER TABLE").is_some() {
        return parse_alter_table(trimmed);
    }
    if strip_prefix_ci(trimmed, "SELECT").is_some() {
        return parse_select(trimmed);
    }
    if strip_prefix_ci(trimmed, "DELETE FROM").is_some() {
        return parse_delete(trimmed);
    }

    Err(FluxError::Parse(format!("unknown statement: {trimmed}")))
}

fn parse_create_table(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "CREATE TABLE")
        .ok_or_else(|| FluxError::Parse("expected CREATE TABLE".to_string()))?
        .trim();

    let open_idx = rest
        .find('(')
        .ok_or_else(|| FluxError::Parse("CREATE TABLE requires column definitions".to_string()))?;
    let close_idx = rest
        .rfind(')')
        .ok_or_else(|| FluxError::Parse("missing ')' in CREATE TABLE".to_string()))?;
    if close_idx <= open_idx {
        return Err(FluxError::Parse(
            "invalid column definition list".to_string(),
        ));
    }
    if !rest[close_idx + 1..].trim().is_empty() {
        return Err(FluxError::Parse(
            "unexpected tokens after CREATE TABLE definition".to_string(),
        ));
    }

    let table_name = normalize_identifier(rest[..open_idx].trim())?;
    let columns_raw = &rest[open_idx + 1..close_idx];
    let column_parts = split_comma_aware(columns_raw)?;
    if column_parts.is_empty() {
        return Err(FluxError::Parse(
            "CREATE TABLE must define at least one column".to_string(),
        ));
    }

    let mut columns = Vec::with_capacity(column_parts.len());
    for part in column_parts {
        let tokens: Vec<&str> = part.split_whitespace().collect();
        if tokens.len() < 2 {
            return Err(FluxError::Parse(format!(
                "invalid column definition: '{part}'"
            )));
        }
        let col_name = tokens[0];
        let type_name = tokens[1];
        let data_type = DataType::parse(type_name)
            .ok_or_else(|| FluxError::Parse(format!("unknown type '{type_name}'")))?;

        let mut constraints = Vec::new();
        let mut t = 2;
        while t < tokens.len() {
            if t + 1 < tokens.len()
                && tokens[t].eq_ignore_ascii_case("PRIMARY")
                && tokens[t + 1].eq_ignore_ascii_case("KEY")
            {
                constraints.push(ColumnConstraint::PrimaryKey);
                t += 2;
            } else if t + 1 < tokens.len()
                && tokens[t].eq_ignore_ascii_case("NOT")
                && tokens[t + 1].eq_ignore_ascii_case("NULL")
            {
                constraints.push(ColumnConstraint::NotNull);
                t += 2;
            } else if tokens[t].eq_ignore_ascii_case("UNIQUE") {
                constraints.push(ColumnConstraint::Unique);
                t += 1;
            } else if tokens[t].eq_ignore_ascii_case("REFERENCES") {
                let (reference, consumed) = parse_references_spec(&tokens[t + 1..])?;
                constraints.push(reference);
                t += 1 + consumed;
            } else {
                return Err(FluxError::Parse(format!(
                    "unknown constraint '{}' in column definition",
                    tokens[t]
                )));
            }
        }

        columns.push(ColumnDef {
            name: normalize_identifier(col_name)?,
            data_type,
            constraints,
        });
    }

    Ok(Statement::CreateTable {
        name: table_name,
        columns,
    })
}

fn parse_references_spec(tokens: &[&str]) -> Result<(ColumnConstraint, usize)> {
    let mut spec = String::new();
    let mut consumed = 0usize;
    for token in tokens {
        spec.push_str(token);
        consumed += 1;
        if spec.contains(')') {
            break;
        }
    }
    let open = spec.find('(');
    let close = spec.rfind(')');
    let (Some(open), Some(close)) = (open, close) else {
        return Err(FluxError::Parse(
            "REFERENCES requires 'table(column)'".to_string(),
        ));
    };
    if close <= open || !spec[close + 1..].trim().is_empty() {
        return Err(FluxError::Parse(
            "REFERENCES requires 'table(column)'".to_string(),
        ));
    }
    let table = normalize_identifier(spec[..open].trim())?;
    let column = normalize_identifier(spec[open + 1..close].trim())?;
    Ok((ColumnConstraint::References { table, column }, consumed))
}

fn parse_create_index(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "CREATE INDEX")
        .ok_or_else(|| FluxError::Parse("expected CREATE INDEX".to_string()))?
        .trim();
    let on_idx = find_keyword_outside_quotes(rest, "ON")
        .ok_or_else(|| FluxError::Parse("CREATE INDEX requires ON clause".to_string()))?;
    let index_name = normalize_identifier(rest[..on_idx].trim())?;

    let on_part = rest[on_idx + "ON".len()..].trim();
    let open_idx = on_part
        .find('(')
        .ok_or_else(|| FluxError::Parse("CREATE INDEX requires '(column)'".to_string()))?;
    let close_idx = on_part
        .rfind(')')
        .ok_or_else(|| FluxError::Parse("missing ')' in CREATE INDEX".to_string()))?;
    if close_idx <= open_idx {
        return Err(FluxError::Parse(
            "invalid CREATE INDEX column definition".to_string(),
        ));
    }
    if !on_part[close_idx + 1..].trim().is_empty() {
        return Err(FluxError::Parse(
            "unexpected tokens after CREATE INDEX".to_string(),
        ));
    }

    let table = normalize_identifier(on_part[..open_idx].trim())?;
    let column = normalize_identifier(on_part[open_idx + 1..close_idx].trim())?;
    Ok(Statement::CreateIndex {
        name: index_name,
        table,
        column,
    })
}

fn parse_drop_table(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "DROP TABLE")
        .ok_or_else(|| FluxError::Parse("expected DROP TABLE".to_string()))?
        .trim();
    if rest.is_empty() {
        return Err(FluxError::Parse(
            "DROP TABLE requires table name".to_string(),
        ));
    }
    let name = normalize_identifier(rest)?;
    Ok(Statement::DropTable { name })
}

fn parse_drop_index(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "DROP INDEX")
        .ok_or_else(|| FluxError::Parse("expected DROP INDEX".to_string()))?
        .trim();
    if rest.is_empty() {
        return Err(FluxError::Parse(
            "DROP INDEX requires index name".to_string(),
        ));
    }
    let name = normalize_identifier(rest)?;
    Ok(Statement::DropIndex { name })
}

fn parse_alter_table(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "ALTER TABLE")
        .ok_or_else(|| FluxError::Parse("expected ALTER TABLE".to_string()))?
        .trim();
    if rest.is_empty() {
        return Err(FluxError::Parse(
            "ALTER TABLE requires table name and operation".to_string(),
        ));
    }

    if let Some(add_col_idx) = find_keyword_outside_quotes(rest, "ADD COLUMN") {
        let table = normalize_identifier(rest[..add_col_idx].trim())?;
        let definition = rest[add_col_idx + "ADD COLUMN".len()..].trim();
        return parse_alter_add_column(table, definition);
    }

    if let Some(drop_col_idx) = find_keyword_outside_quotes(rest, "DROP COLUMN") {
        let table = normalize_identifier(rest[..drop_col_idx].trim())?;
        let column = normalize_identifier(rest[drop_col_idx + "DROP COLUMN".len()..].trim())?;
        return Ok(Statement::AlterTableDropColumn { table, column });
    }

    if let Some(rename_col_idx) = find_keyword_outside_quotes(rest, "RENAME COLUMN") {
        let table = normalize_identifier(rest[..rename_col_idx].trim())?;
        let rename_part = rest[rename_col_idx + "RENAME COLUMN".len()..].trim();
        let to_idx = find_keyword_outside_quotes(rename_part, "TO")
            .ok_or_else(|| FluxError::Parse("RENAME COLUMN requires TO".to_string()))?;
        let old_name = normalize_identifier(rename_part[..to_idx].trim())?;
        let new_name = normalize_identifier(rename_part[to_idx + "TO".len()..].trim())?;
        return Ok(Statement::AlterTableRenameColumn {
            table,
            old_name,
            new_name,
        });
    }

    Err(FluxError::Parse(
        "ALTER TABLE supports: ADD COLUMN, DROP COLUMN, RENAME COLUMN".to_string(),
    ))
}

fn parse_alter_add_column(table: String, definition: &str) -> Result<Statement> {
    if definition.is_empty() {
        return Err(FluxError::Parse(
            "ADD COLUMN requires column definition".to_string(),
        ));
    }

    let (column_def_raw, default_raw) =
        if let Some(default_idx) = find_keyword_outside_quotes(definition, "DEFAULT") {
            (
                definition[..default_idx].trim(),
                Some(definition[default_idx + "DEFAULT".len()..].trim()),
            )
        } else {
            (definition, None)
        };

    let tokens: Vec<&str> = column_def_raw.split_whitespace().collect();
    if tokens.len() < 2 {
        return Err(FluxError::Parse(
            "ADD COLUMN requires name and type".to_string(),
        ));
    }
    let column_name = tokens[0];
    let type_name = tokens[1];

    let mut constraints = Vec::new();
    let mut t = 2;
    while t < tokens.len() {
        if t + 1 < tokens.len()
            && tokens[t].eq_ignore_ascii_case("NOT")
            && tokens[t + 1].eq_ignore_ascii_case("NULL")
        {
            constraints.push(ColumnConstraint::NotNull);
            t += 2;
        } else if tokens[t].eq_ignore_ascii_case("UNIQUE") {
            constraints.push(ColumnConstraint::Unique);
            t += 1;
        } else if tokens[t].eq_ignore_ascii_case("REFERENCES") {
            let (reference, consumed) = parse_references_spec(&tokens[t + 1..])?;
            constraints.push(reference);
            t += 1 + consumed;
        } else {
            return Err(FluxError::Parse(format!(
                "unknown constraint '{}' in ADD COLUMN",
                tokens[t]
            )));
        }
    }

    let data_type = DataType::parse(type_name)
        .ok_or_else(|| FluxError::Parse(format!("unknown type '{type_name}'")))?;
    let default = match default_raw {
        Some(raw) if !raw.is_empty() => Some(parse_literal(raw)?),
        Some(_) => {
            return Err(FluxError::Parse(
                "DEFAULT requires a literal value".to_string(),
            ));
        }
        None => None,
    };

    Ok(Statement::AlterTableAddColumn {
        table,
        column: ColumnDef {
            name: normalize_identifier(column_name)?,
            data_type,
            constraints,
        },
        default,
    })
}

fn parse_insert(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "INSERT INTO")
        .ok_or_else(|| FluxError::Parse("expected INSERT INTO".to_string()))?
        .trim();

    let values_idx = find_keyword_outside_quotes(rest, "VALUES")
        .ok_or_else(|| FluxError::Parse("INSERT requires VALUES clause".to_string()))?;
    let before_values = rest[..values_idx].trim();

    let (table, columns) = if let Some(open_paren) = before_values.find('(') {
        let close_paren = before_values
            .rfind(')')
            .ok_or_else(|| FluxError::Parse("missing ')' in INSERT column list".to_string()))?;
        let table = normalize_identifier(before_values[..open_paren].trim())?;
        let cols_raw = &before_values[open_paren + 1..close_paren];
        let cols = split_comma_aware(cols_raw)?
            .into_iter()
            .map(|c| normalize_identifier(&c))
            .collect::<Result<Vec<_>>>()?;
        if cols.is_empty() {
            return Err(FluxError::Parse("empty column list in INSERT".to_string()));
        }
        (table, Some(cols))
    } else {
        (normalize_identifier(before_values)?, None)
    };

    let values_raw = rest[values_idx + "VALUES".len()..].trim();
    if !values_raw.starts_with('(') || !values_raw.ends_with(')') {
        return Err(FluxError::Parse(
            "VALUES must be enclosed in parentheses".to_string(),
        ));
    }

    let inner = &values_raw[1..values_raw.len() - 1];
    let values = if inner.trim().is_empty() {
        Vec::new()
    } else {
        split_comma_aware(inner)?
            .into_iter()
            .map(|token| parse_literal(&token))
            .collect::<Result<Vec<_>>>()?
    };

    Ok(Statement::Insert {
        table,
        columns,
        values,
    })
}

fn parse_update(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "UPDATE")
        .ok_or_else(|| FluxError::Parse("expected UPDATE".to_string()))?
        .trim();
    let set_idx = find_keyword_outside_quotes(rest, "SET")
        .ok_or_else(|| FluxError::Parse("UPDATE requires SET clause".to_string()))?;
    let table = normalize_identifier(rest[..set_idx].trim())?;

    let after_set = rest[set_idx + "SET".len()..].trim();
    if after_set.is_empty() {
        return Err(FluxError::Parse(
            "missing assignments in UPDATE".to_string(),
        ));
    }

    let (assignments_raw, where_raw) =
        if let Some(where_idx) = find_keyword_outside_quotes(after_set, "WHERE") {
            (
                after_set[..where_idx].trim(),
                Some(after_set[where_idx + "WHERE".len()..].trim()),
            )
        } else {
            (after_set, None)
        };

    let assignment_parts = split_comma_aware(assignments_raw)?;
    if assignment_parts.is_empty() {
        return Err(FluxError::Parse(
            "UPDATE requires at least one assignment".to_string(),
        ));
    }
    let mut assignments = Vec::with_capacity(assignment_parts.len());
    for part in assignment_parts {
        let eq_idx = find_char_outside_quotes(&part, '=')?;
        let column = normalize_identifier(part[..eq_idx].trim())?;
        let value = parse_literal(part[eq_idx + 1..].trim())?;
        assignments.push((column, value));
    }

    let filter = where_raw.map(parse_filter).transpose()?;
    Ok(Statement::Update {
        table,
        assignments,
        filter,
    })
}

fn parse_select(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "SELECT")
        .ok_or_else(|| FluxError::Parse("expected SELECT".to_string()))?
        .trim();

    let from_idx = find_keyword_outside_quotes(rest, "FROM")
        .ok_or_else(|| FluxError::Parse("SELECT requires FROM clause".to_string()))?;
    let columns_raw = rest[..from_idx].trim();
    let after_from = rest[from_idx + "FROM".len()..].trim();
    if after_from.is_empty() {
        return Err(FluxError::Parse("missing table name in SELECT".to_string()));
    }

    let mut remaining = after_from;

    let from_end = find_first_keyword(
        remaining,
        &["WHERE", "GROUP BY", "ORDER BY", "LIMIT", "OFFSET"],
    )
    .unwrap_or(remaining.len());
    let from_part = remaining[..from_end].trim();
    remaining = remaining[from_end..].trim();

    let mut joins = Vec::new();
    let table = if let Some(first_join) = find_keyword_outside_quotes(from_part, "JOIN") {
        let before = &from_part[..first_join];
        let base_end = find_keyword_outside_quotes(before, "INNER").unwrap_or(before.len());
        let table = normalize_identifier(before[..base_end].trim())?;

        let mut cursor = from_part[first_join + "JOIN".len()..].trim();
        loop {
            let seg_end =
                find_first_keyword(cursor, &["INNER", "JOIN"]).unwrap_or(cursor.len());
            let segment = cursor[..seg_end].trim();

            let on_idx = find_keyword_outside_quotes(segment, "ON")
                .ok_or_else(|| FluxError::Parse("JOIN requires ON clause".to_string()))?;
            let join_table = normalize_identifier(segment[..on_idx].trim())?;
            let on_condition = segment[on_idx + "ON".len()..].trim();

            let eq_idx = find_char_outside_quotes(on_condition, '=')?;
            let left_col = parse_qualified_column(on_condition[..eq_idx].trim())?;
            let right_col = parse_qualified_column(on_condition[eq_idx + 1..].trim())?;

            let (left_table, left_column, right_column) =
                if left_col.0.as_deref() == Some(&*join_table) {
                    (right_col.0, right_col.1, left_col.1)
                } else {
                    (left_col.0, left_col.1, right_col.1)
                };

            joins.push(JoinClause {
                table: join_table,
                left_table,
                left_column,
                right_column,
            });

            let next = cursor[seg_end..].trim_start();
            if next.is_empty() {
                break;
            }
            let next = strip_prefix_ci(next, "INNER")
                .map(str::trim_start)
                .unwrap_or(next);
            cursor = strip_prefix_ci(next, "JOIN")
                .ok_or_else(|| FluxError::Parse("expected JOIN clause".to_string()))?
                .trim_start();
        }
        table
    } else {
        normalize_identifier(from_part)?
    };

    let filter = if let Some(where_idx) = find_keyword_outside_quotes(remaining, "WHERE") {
        let after_where = remaining[where_idx + "WHERE".len()..].trim();
        let where_end =
            find_first_keyword(after_where, &["GROUP BY", "ORDER BY", "LIMIT", "OFFSET"])
                .unwrap_or(after_where.len());
        let filter_str = after_where[..where_end].trim();
        remaining = after_where[where_end..].trim();
        Some(parse_filter(filter_str)?)
    } else {
        None
    };

    let group_by = if let Some(group_idx) = find_keyword_outside_quotes(remaining, "GROUP BY") {
        let after_group = remaining[group_idx + "GROUP BY".len()..].trim();
        let group_end =
            find_first_keyword(after_group, &["HAVING", "ORDER BY", "LIMIT", "OFFSET"])
                .unwrap_or(after_group.len());
        let group_str = after_group[..group_end].trim();
        remaining = after_group[group_end..].trim();
        let columns = split_comma_aware(group_str)?
            .into_iter()
            .map(|c| normalize_identifier(&c))
            .collect::<Result<Vec<_>>>()?;
        if columns.is_empty() {
            return Err(FluxError::Parse("empty GROUP BY clause".to_string()));
        }
        columns
    } else {
        Vec::new()
    };

    let having = if let Some(having_idx) = find_keyword_outside_quotes(remaining, "HAVING") {
        let after_having = remaining[having_idx + "HAVING".len()..].trim();
        let having_end = find_first_keyword(after_having, &["ORDER BY", "LIMIT", "OFFSET"])
            .unwrap_or(after_having.len());
        let having_str = after_having[..having_end].trim();
        remaining = after_having[having_end..].trim();
        if group_by.is_empty() {
            return Err(FluxError::Parse("HAVING requires GROUP BY".to_string()));
        }
        Some(parse_having(having_str)?)
    } else {
        None
    };

    let order_by =
        if let Some(order_idx) = find_keyword_outside_quotes(remaining, "ORDER BY") {
            let after_order = remaining[order_idx + "ORDER BY".len()..].trim();
            let order_end = find_first_keyword(after_order, &["LIMIT", "OFFSET"])
                .unwrap_or(after_order.len());
            let order_str = after_order[..order_end].trim();
            remaining = after_order[order_end..].trim();
            parse_order_by(order_str)?
        } else {
            Vec::new()
        };

    let limit = if let Some(limit_idx) = find_keyword_outside_quotes(remaining, "LIMIT") {
        let after_limit = remaining[limit_idx + "LIMIT".len()..].trim();
        let limit_end = find_first_keyword(after_limit, &["OFFSET"]).unwrap_or(after_limit.len());
        let limit_str = after_limit[..limit_end].trim();
        remaining = after_limit[limit_end..].trim();
        Some(
            limit_str
                .parse::<usize>()
                .map_err(|_| FluxError::Parse(format!("invalid LIMIT value: '{limit_str}'")))?,
        )
    } else {
        None
    };

    let offset = if let Some(offset_idx) = find_keyword_outside_quotes(remaining, "OFFSET") {
        let offset_str = remaining[offset_idx + "OFFSET".len()..].trim();
        Some(
            offset_str
                .parse::<usize>()
                .map_err(|_| FluxError::Parse(format!("invalid OFFSET value: '{offset_str}'")))?,
        )
    } else {
        None
    };

    let columns = parse_select_columns(columns_raw)?;

    Ok(Statement::Select {
        table,
        columns,
        joins,
        filter,
        group_by,
        having,
        order_by,
        limit,
        offset,
    })
}

fn parse_select_columns(input: &str) -> Result<Vec<SelectExpr>> {
    let trimmed = input.trim();
    if trimmed == "*" {
        return Ok(vec![SelectExpr::AllColumns]);
    }

    let parts = split_comma_aware(trimmed)?;
    if parts.is_empty() {
        return Err(FluxError::Parse("SELECT column list is empty".to_string()));
    }

    let mut exprs = Vec::with_capacity(parts.len());
    for part in parts {
        let p = part.trim();
        if p == "*" {
            exprs.push(SelectExpr::AllColumns);
            continue;
        }
        if let Some(agg) = try_parse_aggregate(p)? {
            exprs.push(agg);
            continue;
        }
        exprs.push(SelectExpr::Column(normalize_identifier(p)?));
    }
    Ok(exprs)
}

fn try_parse_aggregate(input: &str) -> Result<Option<SelectExpr>> {
    let trimmed = input.trim();
    let open = match trimmed.find('(') {
        Some(i) => i,
        None => return Ok(None),
    };
    if !trimmed.ends_with(')') {
        return Ok(None);
    }

    let func_name = trimmed[..open].trim();
    let func = match func_name.to_ascii_uppercase().as_str() {
        "COUNT" => AggregateFunc::Count,
        "SUM" => AggregateFunc::Sum,
        "MIN" => AggregateFunc::Min,
        "MAX" => AggregateFunc::Max,
        "AVG" => AggregateFunc::Avg,
        _ => return Ok(None),
    };

    let inner = trimmed[open + 1..trimmed.len() - 1].trim();
    let target = if inner == "*" {
        AggregateTarget::Star
    } else {
        AggregateTarget::Column(normalize_identifier(inner)?)
    };

    Ok(Some(SelectExpr::Aggregate { func, target }))
}

fn parse_order_by(input: &str) -> Result<Vec<OrderByClause>> {
    let parts = split_comma_aware(input)?;
    let mut clauses = Vec::with_capacity(parts.len());
    for part in parts {
        let tokens: Vec<&str> = part.split_whitespace().collect();
        if tokens.is_empty() {
            return Err(FluxError::Parse("empty ORDER BY clause".to_string()));
        }
        let column = normalize_identifier(tokens[0])?;
        let order = if tokens.len() > 1 {
            match tokens[1].to_ascii_uppercase().as_str() {
                "ASC" => SortOrder::Asc,
                "DESC" => SortOrder::Desc,
                other => {
                    return Err(FluxError::Parse(format!(
                        "invalid sort order '{other}' (expected ASC or DESC)"
                    )));
                }
            }
        } else {
            SortOrder::Asc
        };
        if tokens.len() > 2 {
            return Err(FluxError::Parse(format!(
                "unexpected tokens in ORDER BY: '{part}'"
            )));
        }
        clauses.push(OrderByClause { column, order });
    }
    Ok(clauses)
}

fn parse_qualified_column(input: &str) -> Result<(Option<String>, String)> {
    let trimmed = input.trim();
    if let Some(dot_idx) = trimmed.find('.') {
        let table = normalize_identifier(trimmed[..dot_idx].trim())?;
        let column = normalize_identifier(trimmed[dot_idx + 1..].trim())?;
        Ok((Some(table), column))
    } else {
        Ok((None, normalize_identifier(trimmed)?))
    }
}

fn find_first_keyword(input: &str, keywords: &[&str]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for kw in keywords {
        if let Some(idx) = find_keyword_outside_quotes(input, kw) {
            if best.is_none_or(|b| idx < b) {
                best = Some(idx);
            }
        }
    }
    best
}

fn parse_delete(input: &str) -> Result<Statement> {
    let rest = strip_prefix_ci(input.trim(), "DELETE FROM")
        .ok_or_else(|| FluxError::Parse("expected DELETE FROM".to_string()))?
        .trim();
    if rest.is_empty() {
        return Err(FluxError::Parse("missing table name in DELETE".to_string()));
    }

    let (table_raw, where_raw) = if let Some(where_idx) = find_keyword_outside_quotes(rest, "WHERE")
    {
        (
            rest[..where_idx].trim(),
            Some(rest[where_idx + "WHERE".len()..].trim()),
        )
    } else {
        (rest, None)
    };

    let table = normalize_identifier(table_raw)?;
    let filter = where_raw.map(parse_filter).transpose()?;
    Ok(Statement::Delete { table, filter })
}

fn parse_filter(input: &str) -> Result<FilterExpr> {
    parse_or_expr(input.trim(), parse_comparison_expr)
}

fn parse_having(input: &str) -> Result<FilterExpr> {
    parse_or_expr(input.trim(), parse_having_comparison)
}

type LeafParser = fn(&str) -> Result<FilterExpr>;

fn parse_or_expr(input: &str, leaf: LeafParser) -> Result<FilterExpr> {
    let parts = split_by_keyword_aware(input, "OR")?;
    if parts.len() <= 1 {
        return parse_and_expr(input, leaf);
    }
    let mut iter = parts.into_iter();
    let first = iter
        .next()
        .ok_or_else(|| FluxError::Parse("empty OR expression".to_string()))?;
    let mut expr = parse_and_expr(&first, leaf)?;
    for part in iter {
        expr = FilterExpr::Or(Box::new(expr), Box::new(parse_and_expr(&part, leaf)?));
    }
    Ok(expr)
}

fn parse_and_expr(input: &str, leaf: LeafParser) -> Result<FilterExpr> {
    let parts = split_by_keyword_aware(input, "AND")?;
    if parts.len() <= 1 {
        return leaf(input);
    }
    let mut iter = parts.into_iter();
    let first = iter
        .next()
        .ok_or_else(|| FluxError::Parse("empty AND expression".to_string()))?;
    let mut expr = leaf(&first)?;
    for part in iter {
        expr = FilterExpr::And(Box::new(expr), Box::new(leaf(&part)?));
    }
    Ok(expr)
}

fn parse_having_comparison(input: &str) -> Result<FilterExpr> {
    let trimmed = input.trim();
    let (idx, op_len, op) = find_comparison_operator(trimmed)?;
    let lhs = trimmed[..idx].trim();
    let column = if let Some(SelectExpr::Aggregate { func, target }) = try_parse_aggregate(lhs)? {
        crate::ast::aggregate_label(&func, &target)
    } else {
        normalize_identifier(lhs)?
    };
    let value = parse_literal(trimmed[idx + op_len..].trim())?;
    Ok(FilterExpr::Compare { column, op, value })
}

fn parse_comparison_expr(input: &str) -> Result<FilterExpr> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(FluxError::Parse("empty WHERE expression".to_string()));
    }

    if let Some(is_not_null_idx) = find_keyword_outside_quotes(trimmed, "IS NOT NULL") {
        let column = normalize_identifier(trimmed[..is_not_null_idx].trim())?;
        let after = trimmed[is_not_null_idx + "IS NOT NULL".len()..].trim();
        if !after.is_empty() {
            return Err(FluxError::Parse(format!(
                "unexpected tokens after IS NOT NULL: '{after}'"
            )));
        }
        return Ok(FilterExpr::IsNotNull { column });
    }

    if let Some(is_null_idx) = find_keyword_outside_quotes(trimmed, "IS NULL") {
        let column = normalize_identifier(trimmed[..is_null_idx].trim())?;
        let after = trimmed[is_null_idx + "IS NULL".len()..].trim();
        if !after.is_empty() {
            return Err(FluxError::Parse(format!(
                "unexpected tokens after IS NULL: '{after}'"
            )));
        }
        return Ok(FilterExpr::IsNull { column });
    }

    let not_in_idx = find_keyword_outside_quotes(trimmed, "NOT IN");
    let in_idx = find_keyword_outside_quotes(trimmed, "IN");
    let in_spec = match (not_in_idx, in_idx) {
        (Some(idx), _) => Some((idx, "NOT IN".len(), true)),
        (None, Some(idx)) => Some((idx, "IN".len(), false)),
        (None, None) => None,
    };
    if let Some((idx, kw_len, negated)) = in_spec {
        let column = normalize_identifier(trimmed[..idx].trim())?;
        let rhs = trimmed[idx + kw_len..].trim();
        if !rhs.starts_with('(') || !rhs.ends_with(')') {
            return Err(FluxError::Parse(
                "IN requires a parenthesized value list or subquery".to_string(),
            ));
        }
        let inner = rhs[1..rhs.len() - 1].trim();
        if strip_prefix_ci(inner, "SELECT").is_some() {
            let subquery = parse_select(inner)?;
            return Ok(FilterExpr::InSubquery {
                column,
                subquery: Box::new(subquery),
                negated,
            });
        }
        let values = split_comma_aware(inner)?
            .into_iter()
            .map(|token| parse_literal(&token))
            .collect::<Result<Vec<_>>>()?;
        if values.is_empty() {
            return Err(FluxError::Parse(
                "IN requires at least one value".to_string(),
            ));
        }
        return Ok(FilterExpr::InList {
            column,
            values,
            negated,
        });
    }

    if let Some(like_idx) = find_keyword_outside_quotes(trimmed, "LIKE") {
        let column = normalize_identifier(trimmed[..like_idx].trim())?;
        let value = parse_literal(trimmed[like_idx + "LIKE".len()..].trim())?;
        return Ok(FilterExpr::Compare {
            column,
            op: CompareOp::Like,
            value,
        });
    }

    let (idx, op_len, op) = find_comparison_operator(trimmed)?;
    let column = normalize_identifier(trimmed[..idx].trim())?;
    let value = parse_literal(trimmed[idx + op_len..].trim())?;
    Ok(FilterExpr::Compare { column, op, value })
}

fn find_comparison_operator(input: &str) -> Result<(usize, usize, CompareOp)> {
    let chars: Vec<char> = input.chars().collect();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' && !in_double {
            if in_single && i + 1 < chars.len() && chars[i + 1] == '\'' {
                i += 2;
                continue;
            }
            in_single = !in_single;
            i += 1;
            continue;
        }
        if ch == '"' && !in_single {
            if in_double && i + 1 < chars.len() && chars[i + 1] == '"' {
                i += 2;
                continue;
            }
            in_double = !in_double;
            i += 1;
            continue;
        }

        if !in_single && !in_double {
            if i + 1 < chars.len() {
                let next = chars[i + 1];
                if ch == '>' && next == '=' {
                    return Ok((i, 2, CompareOp::Gte));
                }
                if ch == '<' && next == '=' {
                    return Ok((i, 2, CompareOp::Lte));
                }
                if ch == '!' && next == '=' {
                    return Ok((i, 2, CompareOp::NotEq));
                }
                if ch == '<' && next == '>' {
                    return Ok((i, 2, CompareOp::NotEq));
                }
            }

            if ch == '=' {
                return Ok((i, 1, CompareOp::Eq));
            }
            if ch == '>' {
                return Ok((i, 1, CompareOp::Gt));
            }
            if ch == '<' {
                return Ok((i, 1, CompareOp::Lt));
            }
        }
        i += 1;
    }

    Err(FluxError::Parse(
        "WHERE comparison must contain one of: =, !=, <>, >, >=, <, <=, LIKE".to_string(),
    ))
}

fn parse_literal(input: &str) -> Result<Value> {
    let token = input.trim();
    if token.eq_ignore_ascii_case("NULL") {
        return Ok(Value::Null);
    }
    if token.eq_ignore_ascii_case("TRUE") {
        return Ok(Value::Bool(true));
    }
    if token.eq_ignore_ascii_case("FALSE") {
        return Ok(Value::Bool(false));
    }
    if let Ok(number) = token.parse::<i64>() {
        return Ok(Value::Int(number));
    }

    if token.starts_with('\'') && token.ends_with('\'') && token.len() >= 2 {
        let inner = &token[1..token.len() - 1];
        return Ok(Value::Text(inner.replace("''", "'")));
    }
    if token.starts_with('"') && token.ends_with('"') && token.len() >= 2 {
        let inner = &token[1..token.len() - 1];
        return Ok(Value::Text(inner.replace("\"\"", "\"")));
    }

    Err(FluxError::Parse(format!("invalid literal '{token}'")))
}

fn split_statements(input: &str) -> Result<Vec<String>> {
    split_aware(input, ';')
}

fn split_comma_aware(input: &str) -> Result<Vec<String>> {
    split_aware(input, ',')
}

fn split_aware(input: &str, delimiter: char) -> Result<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut paren_depth = 0i32;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' && !in_double {
            current.push(ch);
            if in_single && i + 1 < chars.len() && chars[i + 1] == '\'' {
                i += 1;
                current.push(chars[i]);
            } else {
                in_single = !in_single;
            }
            i += 1;
            continue;
        }

        if ch == '"' && !in_single {
            current.push(ch);
            if in_double && i + 1 < chars.len() && chars[i + 1] == '"' {
                i += 1;
                current.push(chars[i]);
            } else {
                in_double = !in_double;
            }
            i += 1;
            continue;
        }

        if !in_single && !in_double {
            if ch == '(' {
                paren_depth += 1;
            } else if ch == ')' {
                paren_depth -= 1;
            }
        }

        if ch == delimiter && !in_single && !in_double && paren_depth == 0 {
            let piece = current.trim();
            if !piece.is_empty() {
                parts.push(piece.to_string());
            }
            current.clear();
            i += 1;
            continue;
        }

        current.push(ch);
        i += 1;
    }

    if in_single || in_double {
        return Err(FluxError::Parse("unterminated quoted string".to_string()));
    }

    let piece = current.trim();
    if !piece.is_empty() {
        parts.push(piece.to_string());
    }

    Ok(parts)
}

fn split_by_keyword_aware(input: &str, keyword: &str) -> Result<Vec<String>> {
    let mut segments = Vec::new();
    let mut cursor = 0usize;
    while let Some(idx_rel) = find_keyword_outside_quotes(&input[cursor..], keyword) {
        let idx = cursor + idx_rel;
        let segment = input[cursor..idx].trim();
        if segment.is_empty() {
            return Err(FluxError::Parse(format!(
                "empty expression around '{keyword}'"
            )));
        }
        segments.push(segment.to_string());
        cursor = idx + keyword.len();
    }
    let tail = input[cursor..].trim();
    if tail.is_empty() {
        return Err(FluxError::Parse(format!(
            "missing expression after '{keyword}'"
        )));
    }
    if !segments.is_empty() {
        segments.push(tail.to_string());
        Ok(segments)
    } else {
        Ok(vec![input.trim().to_string()])
    }
}

fn find_char_outside_quotes(input: &str, target: char) -> Result<usize> {
    let mut in_single = false;
    let mut in_double = false;
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' && !in_double {
            if in_single && i + 1 < chars.len() && chars[i + 1] == '\'' {
                i += 2;
                continue;
            }
            in_single = !in_single;
            i += 1;
            continue;
        }
        if ch == '"' && !in_single {
            if in_double && i + 1 < chars.len() && chars[i + 1] == '"' {
                i += 2;
                continue;
            }
            in_double = !in_double;
            i += 1;
            continue;
        }
        if ch == target && !in_single && !in_double {
            return Ok(i);
        }
        i += 1;
    }

    Err(FluxError::Parse(format!(
        "expected '{target}' in expression"
    )))
}

fn find_keyword_outside_quotes(input: &str, keyword: &str) -> Option<usize> {
    let bytes = input.as_bytes();
    let key = keyword.as_bytes();
    let mut i = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut paren_depth = 0i32;

    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\'' && !in_double {
            if in_single && i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                i += 2;
                continue;
            }
            in_single = !in_single;
            i += 1;
            continue;
        }
        if b == b'"' && !in_single {
            if in_double && i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                i += 2;
                continue;
            }
            in_double = !in_double;
            i += 1;
            continue;
        }

        if !in_single && !in_double {
            if b == b'(' {
                paren_depth += 1;
            } else if b == b')' {
                paren_depth -= 1;
            }
        }

        if !in_single
            && !in_double
            && paren_depth == 0
            && i + key.len() <= bytes.len()
            && bytes[i..i + key.len()].eq_ignore_ascii_case(key)
        {
            let prev = if i == 0 { None } else { Some(bytes[i - 1]) };
            let next = if i + key.len() == bytes.len() {
                None
            } else {
                Some(bytes[i + key.len()])
            };
            if is_boundary(prev) && is_boundary(next) {
                return Some(i);
            }
        }
        i += 1;
    }

    None
}

fn is_boundary(byte: Option<u8>) -> bool {
    byte.is_none_or(|b| b.is_ascii_whitespace() || matches!(b, b'(' | b')' | b',' | b';'))
}

fn is_exact_keywords(input: &str, keywords: &[&str]) -> bool {
    let tokens: Vec<&str> = input.split_whitespace().collect();
    tokens.len() == keywords.len()
        && tokens
            .iter()
            .zip(keywords.iter())
            .all(|(token, keyword)| token.eq_ignore_ascii_case(keyword))
}

fn strip_prefix_ci<'a>(input: &'a str, prefix: &str) -> Option<&'a str> {
    if input.len() < prefix.len() {
        return None;
    }
    let candidate = &input[..prefix.len()];
    if candidate.eq_ignore_ascii_case(prefix) {
        Some(&input[prefix.len()..])
    } else {
        None
    }
}

fn normalize_identifier(input: &str) -> Result<String> {
    let ident = input.trim();
    if ident.is_empty() {
        return Err(FluxError::Parse("identifier cannot be empty".to_string()));
    }

    if ident.starts_with('"') && ident.ends_with('"') && ident.len() >= 2 {
        return Ok(ident[1..ident.len() - 1].to_string());
    }

    if !ident
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return Err(FluxError::Parse(format!("invalid identifier '{ident}'")));
    }

    Ok(ident.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_create_table() {
        let statement = parse_statement("CREATE TABLE users (id INT, name TEXT, active BOOL)")
            .expect("CREATE TABLE should parse");
        let Statement::CreateTable { name, columns } = statement else {
            panic!("expected create table");
        };
        assert_eq!(name, "users");
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0].name, "id");
    }

    #[test]
    fn parses_create_table_with_constraints() {
        let statement = parse_statement(
            "CREATE TABLE users (id INT PRIMARY KEY, name TEXT NOT NULL UNIQUE, active BOOL)",
        )
        .expect("CREATE TABLE with constraints should parse");
        let Statement::CreateTable { columns, .. } = statement else {
            panic!("expected create table");
        };
        assert!(columns[0]
            .constraints
            .contains(&ColumnConstraint::PrimaryKey));
        assert!(columns[1].constraints.contains(&ColumnConstraint::NotNull));
        assert!(columns[1].constraints.contains(&ColumnConstraint::Unique));
        assert!(columns[2].constraints.is_empty());
    }

    #[test]
    fn parses_insert_with_columns() {
        let statement =
            parse_statement("INSERT INTO users (id, name) VALUES (1, 'Alice')").expect("parse");
        let Statement::Insert {
            table,
            columns,
            values,
        } = statement
        else {
            panic!("expected insert");
        };
        assert_eq!(table, "users");
        assert_eq!(columns, Some(vec!["id".to_string(), "name".to_string()]));
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn parses_insert_with_string_commas() {
        let statement = parse_statement("INSERT INTO users VALUES (1, 'Doe, John', true)")
            .expect("INSERT should parse");
        let Statement::Insert {
            table,
            columns,
            values,
        } = statement
        else {
            panic!("expected insert");
        };
        assert_eq!(table, "users");
        assert!(columns.is_none());
        assert_eq!(values.len(), 3);
        assert_eq!(values[1], Value::Text("Doe, John".to_string()));
    }

    #[test]
    fn parses_script_multiple_statements() {
        let statements = parse_script(
            "CREATE TABLE users (id INT); INSERT INTO users VALUES (1); SELECT * FROM users;",
        )
        .expect("script should parse");
        assert_eq!(statements.len(), 3);
    }

    #[test]
    fn parses_select_with_order_limit_offset() {
        let statement =
            parse_statement("SELECT id, name FROM users ORDER BY id DESC LIMIT 10 OFFSET 5")
                .expect("parse");
        let Statement::Select {
            order_by,
            limit,
            offset,
            ..
        } = statement
        else {
            panic!("expected select");
        };
        assert_eq!(order_by.len(), 1);
        assert_eq!(order_by[0].column, "id");
        assert!(matches!(order_by[0].order, SortOrder::Desc));
        assert_eq!(limit, Some(10));
        assert_eq!(offset, Some(5));
    }

    #[test]
    fn parses_select_with_aggregates() {
        let statement =
            parse_statement("SELECT COUNT(*), SUM(id), AVG(id) FROM users").expect("parse");
        let Statement::Select { columns, .. } = statement else {
            panic!("expected select");
        };
        assert_eq!(columns.len(), 3);
        assert!(matches!(
            &columns[0],
            SelectExpr::Aggregate {
                func: AggregateFunc::Count,
                target: AggregateTarget::Star
            }
        ));
    }

    #[test]
    fn parses_drop_table() {
        let statement = parse_statement("DROP TABLE users").expect("parse");
        assert!(matches!(statement, Statement::DropTable { name } if name == "users"));
    }

    #[test]
    fn parses_drop_index() {
        let statement = parse_statement("DROP INDEX idx_users_id").expect("parse");
        assert!(matches!(statement, Statement::DropIndex { name } if name == "idx_users_id"));
    }

    #[test]
    fn parses_alter_table_drop_column() {
        let statement = parse_statement("ALTER TABLE users DROP COLUMN email").expect("parse");
        assert!(matches!(
            statement,
            Statement::AlterTableDropColumn { table, column } if table == "users" && column == "email"
        ));
    }

    #[test]
    fn parses_alter_table_rename_column() {
        let statement =
            parse_statement("ALTER TABLE users RENAME COLUMN name TO full_name").expect("parse");
        assert!(matches!(
            statement,
            Statement::AlterTableRenameColumn { table, old_name, new_name }
                if table == "users" && old_name == "name" && new_name == "full_name"
        ));
    }

    #[test]
    fn parses_begin_commit_rollback() {
        assert!(matches!(
            parse_statement("BEGIN").unwrap(),
            Statement::Begin
        ));
        assert!(matches!(
            parse_statement("COMMIT").unwrap(),
            Statement::Commit
        ));
        assert!(matches!(
            parse_statement("ROLLBACK").unwrap(),
            Statement::Rollback
        ));
    }

    #[test]
    fn parses_select_with_where_and_and_or() {
        let statement = parse_statement(
            "SELECT id, name FROM users WHERE id >= 1 AND name LIKE 'A%' OR active = true",
        )
        .expect("SELECT should parse");
        let Statement::Select {
            table,
            columns,
            filter,
            ..
        } = statement
        else {
            panic!("expected select");
        };
        assert_eq!(table, "users");
        assert_eq!(columns.len(), 2);
        assert!(matches!(filter, Some(FilterExpr::Or(_, _))));
    }

    #[test]
    fn parses_alter_table_add_column_with_default() {
        let statement = parse_statement("ALTER TABLE users ADD COLUMN verified BOOL DEFAULT false")
            .expect("ALTER TABLE should parse");
        let Statement::AlterTableAddColumn {
            table,
            column,
            default,
        } = statement
        else {
            panic!("expected ALTER TABLE ADD COLUMN");
        };
        assert_eq!(table, "users");
        assert_eq!(column.name, "verified");
        assert_eq!(column.data_type, DataType::Bool);
        assert_eq!(default, Some(Value::Bool(false)));
    }

    #[test]
    fn parses_update_statement() {
        let statement = parse_statement(
            "UPDATE users SET name = 'Aneta', active = false WHERE id = 10 AND active = true",
        )
        .expect("UPDATE should parse");
        let Statement::Update {
            table,
            assignments,
            filter,
        } = statement
        else {
            panic!("expected update");
        };
        assert_eq!(table, "users");
        assert_eq!(assignments.len(), 2);
        assert!(matches!(filter, Some(FilterExpr::And(_, _))));
    }

    #[test]
    fn parses_create_index() {
        let statement =
            parse_statement("CREATE INDEX idx_users_id ON users (id)").expect("create index");
        let Statement::CreateIndex {
            name,
            table,
            column,
        } = statement
        else {
            panic!("expected create index");
        };
        assert_eq!(name, "idx_users_id");
        assert_eq!(table, "users");
        assert_eq!(column, "id");
    }

    #[test]
    fn parses_show_migrations() {
        let statement = parse_statement("SHOW MIGRATIONS").expect("SHOW MIGRATIONS should parse");
        assert!(matches!(statement, Statement::ShowMigrations));
    }

    #[test]
    fn parses_select_with_join() {
        let statement = parse_statement(
            "SELECT id, name FROM users JOIN orders ON users.id = orders.user_id WHERE id > 0",
        )
        .expect("parse");
        let Statement::Select {
            table,
            joins,
            filter,
            ..
        } = statement
        else {
            panic!("expected select");
        };
        assert_eq!(table, "users");
        assert_eq!(joins.len(), 1);
        assert_eq!(joins[0].table, "orders");
        assert_eq!(joins[0].left_column, "id");
        assert_eq!(joins[0].right_column, "user_id");
        assert!(filter.is_some());
    }

    #[test]
    fn parses_multi_join() {
        let statement = parse_statement(
            "SELECT name FROM users JOIN orders ON users.id = orders.user_id \
             JOIN items ON orders.id = items.order_id",
        )
        .expect("parse");
        let Statement::Select { joins, .. } = statement else {
            panic!("expected select");
        };
        assert_eq!(joins.len(), 2);
        assert_eq!(joins[1].table, "items");
    }

    #[test]
    fn parses_group_by_having() {
        let statement = parse_statement(
            "SELECT user_id, COUNT(*) FROM orders GROUP BY user_id HAVING COUNT(*) > 1",
        )
        .expect("parse");
        let Statement::Select {
            group_by, having, ..
        } = statement
        else {
            panic!("expected select");
        };
        assert_eq!(group_by, vec!["user_id".to_string()]);
        assert!(matches!(
            having,
            Some(FilterExpr::Compare { column, .. }) if column == "COUNT(*)"
        ));
    }

    #[test]
    fn parses_in_list_and_subquery() {
        let statement =
            parse_statement("SELECT * FROM users WHERE id IN (1, 2, 3)").expect("parse");
        let Statement::Select { filter, .. } = statement else {
            panic!("expected select");
        };
        assert!(matches!(
            filter,
            Some(FilterExpr::InList { ref values, negated: false, .. }) if values.len() == 3
        ));

        let statement = parse_statement(
            "SELECT * FROM users WHERE id NOT IN (SELECT user_id FROM banned WHERE active = true)",
        )
        .expect("parse");
        let Statement::Select { filter, .. } = statement else {
            panic!("expected select");
        };
        assert!(matches!(
            filter,
            Some(FilterExpr::InSubquery { negated: true, .. })
        ));
    }

    #[test]
    fn parses_references_constraint() {
        let statement = parse_statement(
            "CREATE TABLE orders (id INT PRIMARY KEY, user_id INT REFERENCES users(id))",
        )
        .expect("parse");
        let Statement::CreateTable { columns, .. } = statement else {
            panic!("expected create table");
        };
        assert!(columns[1].constraints.iter().any(|c| matches!(
            c,
            ColumnConstraint::References { table, column } if table == "users" && column == "id"
        )));
    }
}
