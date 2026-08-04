use rusqlite::Connection;

use super::{INITIAL_SCHEMA, LATEST_SCHEMA_VERSION, StoreError, schema_version};

type SchemaContractEntry = (String, String, String, String);

pub(super) fn validate_exact_schema(connection: &Connection) -> Result<(), StoreError> {
    let expected = expected_schema_contract()?;
    if schema_contract(connection)? != expected {
        return Err(StoreError::InvalidSchema);
    }
    match schema_version(connection) {
        Ok(LATEST_SCHEMA_VERSION) => Ok(()),
        Ok(_) | Err(_) => Err(StoreError::InvalidSchema),
    }
}

fn schema_contract(connection: &Connection) -> Result<Vec<SchemaContractEntry>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '')
         FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%'
         ORDER BY type, name, tbl_name, sql",
    )?;
    Ok(statement
        .query_map([], |row| {
            let sql: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                normalize_schema_sql(&sql),
            ))
        })?
        .collect::<Result<_, _>>()?)
}

pub(super) fn normalize_schema_sql(sql: &str) -> String {
    if sql.is_empty() {
        return String::new();
    }

    let mut normalized = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut pending_space = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                push_pending_space(&mut normalized, &mut pending_space);
                normalized.push('\'');
                copy_quoted(&mut chars, &mut normalized, '\'', Some('\''));
            }
            '"' => {
                push_pending_space(&mut normalized, &mut pending_space);
                normalized.push('"');
                copy_quoted(&mut chars, &mut normalized, '"', Some('"'));
            }
            '[' => {
                push_pending_space(&mut normalized, &mut pending_space);
                normalized.push('[');
                copy_quoted(&mut chars, &mut normalized, ']', None);
            }
            '`' => {
                push_pending_space(&mut normalized, &mut pending_space);
                normalized.push('`');
                copy_quoted(&mut chars, &mut normalized, '`', Some('`'));
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for comment in chars.by_ref() {
                    if comment == '\n' {
                        pending_space = true;
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(comment) = chars.next() {
                    if comment == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        pending_space = true;
                        break;
                    }
                }
            }
            ch if ch.is_whitespace() => pending_space = true,
            ch => {
                push_pending_space(&mut normalized, &mut pending_space);
                normalized.push(ch);
            }
        }
    }

    normalized.trim().to_owned()
}

fn push_pending_space(normalized: &mut String, pending_space: &mut bool) {
    if *pending_space {
        normalized.push(' ');
        *pending_space = false;
    }
}

fn copy_quoted(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    normalized: &mut String,
    closing: char,
    escaped: Option<char>,
) {
    while let Some(inner) = chars.next() {
        normalized.push(inner);
        if inner == closing {
            if escaped.is_some_and(|escaped| chars.peek() == Some(&escaped)) {
                normalized.push(chars.next().expect("peeked escaped delimiter"));
            } else {
                break;
            }
        }
    }
}

fn expected_schema_contract() -> Result<Vec<SchemaContractEntry>, StoreError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(INITIAL_SCHEMA)?;
    schema_contract(&connection)
}

#[cfg(test)]
mod tests {
    use super::normalize_schema_sql;

    #[test]
    fn normalization_should_follow_sqlite_quoting_and_comment_rules() {
        let cases = [
            (
                "CREATE  TABLE\nfoo ( id INTEGER )",
                "CREATE TABLE foo ( id INTEGER )",
            ),
            (
                "SELECT 'a''  b', \"c\"\"  d\"",
                "SELECT 'a''  b', \"c\"\"  d\"",
            ),
            (
                "SELECT [provider  name], `a``  b`",
                "SELECT [provider  name], `a``  b`",
            ),
            ("SELECT X'CA  FE'", "SELECT X'CA  FE'"),
            ("SELECT/* block */1 -- line\n+ 2", "SELECT 1 + 2"),
            ("SELECT [a]]  b]", "SELECT [a]] b]"),
            (
                "SELECT 'unterminated  literal",
                "SELECT 'unterminated  literal",
            ),
        ];

        for (input, expected) in cases {
            assert_eq!(normalize_schema_sql(input), expected, "input: {input}");
        }
    }

    #[test]
    fn normalization_should_distinguish_literal_whitespace() {
        let with_space = "SELECT RAISE(ABORT, 'provider capability is invalid')";
        let with_newline = "SELECT RAISE(ABORT, 'provider\ncapability is invalid')";

        assert_ne!(
            normalize_schema_sql(with_space),
            normalize_schema_sql(with_newline)
        );
    }
}
