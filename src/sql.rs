//! SQL: reads, writes, bound parameters and transactions.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::Client;
use crate::error::{Error, Result};
use crate::params::Param;
use crate::response::Response;

/// The refusal when the server did not grant server-side binding. One wording,
/// so it is searchable.
const NO_SERVER_PARAMS: &str = "this server did not grant server-side parameters (SERVER_PARAMS \
was not in the granted feature set), so this client will not bind `?` placeholders on this \
connection. It will not render the values into the statement text instead: escaping and binding \
are not the same guarantee. Upgrade the server, or build the statement yourself";

/// A SQL result set: the column names, and each row's values as the server
/// rendered them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct Rows {
    /// Column names, in the order the values come in.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub columns: Vec<String>,
    /// One entry per row.
    #[serde(default, deserialize_with = "crate::serde_null::or_default")]
    pub rows: Vec<Vec<String>>,
}

impl Rows {
    /// How many rows came back.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the result set is empty.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// One row's value for a column, by name.
    pub fn get(&self, row: usize, column: &str) -> Option<&str> {
        let index = self.columns.iter().position(|c| c == column)?;
        self.rows.get(row)?.get(index).map(String::as_str)
    }

    /// Every row as a map keyed by column name, for name-based access.
    pub fn maps(&self) -> Vec<HashMap<&str, &str>> {
        self.rows
            .iter()
            .map(|row| {
                self.columns
                    .iter()
                    .zip(row.iter())
                    .map(|(c, v)| (c.as_str(), v.as_str()))
                    .collect()
            })
            .collect()
    }
}

/// One statement of a transaction script, with the values bound to its
/// placeholders.
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    /// The statement text, with `?` placeholders.
    pub sql: String,
    /// The values for those placeholders, in order.
    pub params: Vec<Param>,
}

impl Statement {
    /// A statement with no parameters.
    pub fn new(sql: impl Into<String>) -> Self {
        Self {
            sql: sql.into(),
            params: Vec::new(),
        }
    }

    /// A statement and the values bound to it.
    ///
    /// ```
    /// use tricoredb::{params, Statement};
    /// let s = Statement::with_params("INSERT INTO t VALUES (?, ?)", params![1, "ada"]);
    /// assert_eq!(s.params.len(), 2);
    /// ```
    pub fn with_params(sql: impl Into<String>, params: impl Into<Vec<Param>>) -> Self {
        Self {
            sql: sql.into(),
            params: params.into(),
        }
    }
}

impl From<&str> for Statement {
    fn from(sql: &str) -> Self {
        Statement::new(sql)
    }
}

/// What the server did with a transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct TransactionResult {
    /// How many statements ran.
    #[serde(default)]
    pub statements: i64,
    /// How many writes were made durable.
    #[serde(default)]
    pub committed_writes: i64,
    /// How many buffered writes were thrown away.
    #[serde(default)]
    pub discarded_writes: i64,
    /// `"began"`, `"committed"` or `"rolled_back"`.
    #[serde(default, rename = "transaction")]
    pub outcome: String,
}

impl Client {
    /// Run a statement that is not a `SELECT`: DDL, `INSERT`, `UPDATE`,
    /// `DELETE`, or a transaction script.
    pub fn execute(&mut self, sql: &str) -> Result<Response> {
        let body = self.sql_body(sql, &[])?;
        self.send(json!({"Sql": {"Exec": body}}))
    }

    /// Run a statement with its values bound **by the server**.
    ///
    /// The values travel beside the statement and are substituted at value
    /// positions the grammar has already fixed, so a value cannot become syntax
    /// however it is spelled — a quote, a backslash, or an argument that is
    /// itself a complete SQL statement is stored as the text it is.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// # let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
    /// use tricoredb::params;
    /// db.execute_params("INSERT INTO users VALUES (?, ?)", &params![1, "O'Hara"])?;
    /// # Ok(()) }
    /// ```
    ///
    /// This needs the `SERVER_PARAMS` capability
    /// ([`Client::server_params_granted`]). Against a server that did not grant
    /// it, the call fails before anything is sent rather than falling back to
    /// building the statement text here.
    pub fn execute_params(&mut self, sql: &str, params: &[Param]) -> Result<Response> {
        let body = self.sql_body(sql, params)?;
        self.send(json!({"Sql": {"Exec": body}}))
    }

    /// Run a `SELECT`. The server refuses a write sent this way.
    pub fn query(&mut self, sql: &str) -> Result<Rows> {
        let body = self.sql_body(sql, &[])?;
        let response = self.send(json!({"Sql": {"Query": body}}))?;
        rows_of(&response, "query")
    }

    /// Run a `SELECT` with its values bound by the server. See
    /// [`Client::execute_params`].
    pub fn query_params(&mut self, sql: &str, params: &[Param]) -> Result<Rows> {
        let body = self.sql_body(sql, params)?;
        let response = self.send(json!({"Sql": {"Query": body}}))?;
        rows_of(&response, "query")
    }

    /// Build the `{sql, params}` body.
    ///
    /// How many placeholders a statement has is left to the server: it counts
    /// them against the parameters it was given and refuses a mismatch by name,
    /// and it also understands `$n`, which a client-side scan for `?` would
    /// miscount. One authority on that question is the point.
    fn sql_body(&self, sql: &str, params: &[Param]) -> Result<Value> {
        if params.is_empty() {
            return Ok(json!({"sql": sql}));
        }
        if !self.server_params_granted() {
            return Err(Error::feature_refusal(NO_SERVER_PARAMS));
        }
        for (index, param) in params.iter().enumerate() {
            param.validate(index)?;
        }
        Ok(json!({"sql": sql, "params": params}))
    }

    /// Run a whole `BEGIN … COMMIT` script in **one request**.
    ///
    /// One round trip, one replication event, and it works on every node,
    /// including those that withhold session transactions. Use it when every
    /// statement is known up front; use [`Client::begin`] when a later statement
    /// depends on what an earlier one read.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// # let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
    /// use tricoredb::{params, Statement};
    /// let result = db.transaction(&[
    ///     Statement::with_params("UPDATE accounts SET balance = balance - ? WHERE id = ?", params![10, 1]),
    ///     Statement::with_params("UPDATE accounts SET balance = balance + ? WHERE id = ?", params![10, 2]),
    /// ])?;
    /// assert_eq!(result.outcome, "committed");
    /// # Ok(()) }
    /// ```
    pub fn transaction(&mut self, statements: &[Statement]) -> Result<TransactionResult> {
        let (script, params) = build_transaction_script(statements)?;
        let response = self.execute_params(&script, &params)?;
        response.decode("transaction")
    }

    /// Open a transaction that stays open across requests on this connection.
    ///
    /// Every statement until [`Client::commit`] or [`Client::rollback`] runs
    /// inside it, at one snapshot, invisible to other connections until
    /// committed. The transaction belongs to this connection's socket: another
    /// connection cannot commit it, and a dropped socket rolls it back.
    ///
    /// Needs the `SESSION_TXN` capability ([`Client::session_txn_granted`]).
    /// Without it this fails before writing anything, rather than sending a
    /// `BEGIN` the server would run as a one-statement script;
    /// [`Client::transaction`] works everywhere.
    pub fn begin(&mut self) -> Result<TransactionResult> {
        if !self.session_txn_granted() {
            return Err(Error::feature_refusal(
                "this server did not grant session transactions (SESSION_TXN was not in the \
granted feature set), so begin/commit/rollback cannot open a transaction on this connection. \
Use transaction(...) to send the whole unit as one request",
            ));
        }
        self.transaction_control("BEGIN")
    }

    /// Commit the transaction opened by [`Client::begin`].
    pub fn commit(&mut self) -> Result<TransactionResult> {
        self.transaction_control("COMMIT")
    }

    /// Discard the transaction opened by [`Client::begin`].
    pub fn rollback(&mut self) -> Result<TransactionResult> {
        self.transaction_control("ROLLBACK")
    }

    /// Run a closure inside a transaction: commit when it returns `Ok`, roll
    /// back when it returns `Err`.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// # let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
    /// use tricoredb::params;
    /// db.with_transaction(|tx| {
    ///     tx.execute_params("UPDATE accounts SET balance = balance - ? WHERE id = ?", &params![10, 1])?;
    ///     tx.execute_params("UPDATE accounts SET balance = balance + ? WHERE id = ?", &params![10, 2])?;
    ///     Ok(())
    /// })?;
    /// # Ok(()) }
    /// ```
    ///
    /// The closure must send its statements on the client it is given. A
    /// statement on any other connection is outside the transaction.
    pub fn with_transaction<T>(
        &mut self,
        work: impl FnOnce(&mut Client) -> Result<T>,
    ) -> Result<T> {
        self.begin()?;
        match work(self) {
            Ok(value) => {
                self.commit()?;
                Ok(value)
            }
            Err(error) => {
                // The caller's error is the one that matters; the rollback is
                // best-effort because the server ends the transaction anyway
                // when this connection goes.
                if self.in_transaction() {
                    let _ = self.rollback();
                }
                Err(error)
            }
        }
    }

    /// Send one transaction-control keyword.
    ///
    /// Control travels as `Exec`, because the server authorizes it as a write.
    /// Every `COMMIT`/`ROLLBACK` reply — a refusal included — ends the
    /// transaction, since the server ends it either way. The exception is a
    /// request that never left this process, which stays as it was.
    fn transaction_control(&mut self, keyword: &str) -> Result<TransactionResult> {
        match self.execute(keyword) {
            Ok(response) => {
                self.set_txn_open(keyword == "BEGIN");
                response.decode(keyword)
            }
            Err(error) => {
                if keyword != "BEGIN" && !error.was_refused_locally() {
                    self.set_txn_open(false);
                }
                Err(error)
            }
        }
    }
}

fn rows_of(response: &Response, what: &str) -> Result<Rows> {
    let data = response.expect("Rows", what)?;
    serde_json::from_value(data.clone())
        .map_err(|e| Error::protocol(format!("malformed Rows payload: {e}")))
}

/// Assemble `BEGIN; …; COMMIT` and the flat parameter list that goes with it.
///
/// The parameters of every statement are concatenated in statement order, which
/// is how the server binds a multi-statement script: it walks the script left to
/// right and takes one parameter per placeholder. So the script keeps its
/// placeholders instead of having values pasted into it.
pub(crate) fn build_transaction_script(statements: &[Statement]) -> Result<(String, Vec<Param>)> {
    if statements.is_empty() {
        return Err(Error::invalid("a transaction needs at least one statement"));
    }
    let mut parts = Vec::with_capacity(statements.len());
    let mut params = Vec::new();
    for statement in statements {
        let text = statement.sql.trim().trim_end_matches(';').trim();
        if text.is_empty() {
            return Err(Error::invalid(
                "every statement in a transaction must be non-empty SQL",
            ));
        }
        let first = text
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        // Caller-supplied transaction control changes what the script means in
        // a way the caller almost certainly did not intend.
        if matches!(first.as_str(), "BEGIN" | "START" | "COMMIT" | "ROLLBACK") {
            return Err(Error::invalid(format!(
                "transaction() brackets the script itself — remove the `{first}` statement"
            )));
        }
        parts.push(text.to_string());
        params.extend(statement.params.iter().cloned());
    }
    Ok((format!("BEGIN; {}; COMMIT", parts.join("; ")), params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params;

    #[test]
    fn a_script_keeps_its_placeholders_and_flattens_its_parameters() {
        let (script, params) = build_transaction_script(&[
            Statement::with_params("INSERT INTO t VALUES (?, ?)", params![1, "ada"]),
            Statement::with_params("UPDATE t SET name = ? WHERE id = ?", params!["bob", 1]),
        ])
        .unwrap();
        assert_eq!(
            script,
            "BEGIN; INSERT INTO t VALUES (?, ?); UPDATE t SET name = ? WHERE id = ?; COMMIT"
        );
        assert_eq!(params.len(), 4);
        assert_eq!(params[1], Param::Text("ada".into()));
        assert_eq!(params[2], Param::Text("bob".into()));
    }

    #[test]
    fn trailing_semicolons_and_whitespace_do_not_break_the_script() {
        let (script, _) =
            build_transaction_script(&[Statement::new("  INSERT INTO t VALUES (1) ;  ")]).unwrap();
        assert_eq!(script, "BEGIN; INSERT INTO t VALUES (1); COMMIT");
    }

    #[test]
    fn caller_supplied_transaction_control_is_refused() {
        for sql in ["BEGIN", "begin", "COMMIT", "ROLLBACK", "START TRANSACTION"] {
            let err = build_transaction_script(&[Statement::new(sql)]).unwrap_err();
            assert_eq!(err.kind, crate::ErrorKind::InvalidArgument);
        }
    }

    #[test]
    fn an_empty_transaction_is_refused() {
        assert!(build_transaction_script(&[]).is_err());
        assert!(build_transaction_script(&[Statement::new("  ;  ")]).is_err());
    }

    #[test]
    fn rows_can_be_read_by_column_name() {
        let rows = Rows {
            columns: vec!["id".into(), "name".into()],
            rows: vec![vec!["1".into(), "ada".into()]],
        };
        assert_eq!(rows.get(0, "name"), Some("ada"));
        assert_eq!(rows.get(0, "missing"), None);
        assert_eq!(rows.maps()[0]["id"], "1");
        assert_eq!(rows.len(), 1);
        assert!(!rows.is_empty());
    }
}
