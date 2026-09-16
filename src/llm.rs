//! Read-only context export for language models, and the two admin reads.

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::client::Client;
use crate::error::{Error, Result};

/// How the server should render an export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    /// TriCoreDB's own representation.
    Native,
    /// Standard JSON.
    Json,
    /// The token-oriented rendering meant for a model's context window.
    #[default]
    Toon,
    /// Markdown, for a person to read.
    Markdown,
}

/// What to include in an export, and what to hide.
///
/// [`LlmOptions::default`] matches the server's own defaults: redact, no schema,
/// no row cap. Build from it rather than from a zeroed struct, so redaction is
/// never turned off by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmOptions {
    /// Cap the rows included. `None` leaves the cap to the server.
    pub max_rows: Option<usize>,
    /// Redact sensitive fields before exporting.
    pub redact_sensitive: bool,
    /// Include type information alongside the data.
    pub include_schema: bool,
}

impl Default for LlmOptions {
    fn default() -> Self {
        Self {
            max_rows: None,
            redact_sensitive: true,
            include_schema: false,
        }
    }
}

impl LlmOptions {
    /// Cap the rows included.
    pub fn max_rows(mut self, rows: usize) -> Self {
        self.max_rows = Some(rows);
        self
    }

    /// Turn redaction on or off. **Off means sensitive values are exported as
    /// they are stored.**
    pub fn redact_sensitive(mut self, redact: bool) -> Self {
        self.redact_sensitive = redact;
        self
    }

    /// Include type information.
    pub fn include_schema(mut self, include: bool) -> Self {
        self.include_schema = include;
        self
    }

    fn to_wire(self) -> Value {
        json!({
            "max_rows": self.max_rows.filter(|r| *r > 0),
            "redact_sensitive": self.redact_sensitive,
            "include_schema": self.include_schema,
        })
    }
}

/// One read-only source contributing to a context bundle.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct LlmSource(Value);

impl LlmSource {
    /// A `SELECT`. The caller needs permission to read it.
    pub fn sql(query: &str) -> Self {
        LlmSource(json!({"Sql": {"query": query}}))
    }

    /// A document query. The caller needs permission to read the collection.
    pub fn document_find(
        collection: &str,
        filter: &crate::DocumentFilter,
        limit: Option<usize>,
    ) -> Self {
        LlmSource(json!({"DocumentFind": {
            "collection": collection,
            "filter": filter,
            "limit": limit.filter(|l| *l > 0),
        }}))
    }
}

impl Client {
    /// Assemble a context bundle from one or more read-only sources.
    ///
    /// The result is the rendered bundle: text for TOON and Markdown, JSON
    /// otherwise.
    ///
    /// ```no_run
    /// # fn main() -> tricoredb::Result<()> {
    /// # let mut db = tricoredb::Client::connect(&tricoredb::Options::default())?;
    /// use tricoredb::{LlmSource, OutputFormat};
    /// let bundle = db.llm_context(
    ///     &[LlmSource::sql("SELECT id, name FROM users")],
    ///     OutputFormat::Toon,
    ///     None,
    /// )?;
    /// # let _ = bundle; Ok(()) }
    /// ```
    pub fn llm_context(
        &mut self,
        sources: &[LlmSource],
        format: OutputFormat,
        options: Option<LlmOptions>,
    ) -> Result<String> {
        if sources.is_empty() {
            return Err(Error::invalid("a context bundle needs at least one source"));
        }
        let response = self.send(json!({"Llm": {"Context": {
            "sources": sources,
            "format": format,
            "options": options.unwrap_or_default().to_wire(),
        }}}))?;
        rendered(&response)
    }

    /// Export the schema catalog: SQL tables and document collections.
    pub fn llm_schema(
        &mut self,
        format: OutputFormat,
        options: Option<LlmOptions>,
    ) -> Result<String> {
        let response = self.send(json!({"Llm": {"Schema": {
            "format": format,
            "options": options.unwrap_or_default().to_wire(),
        }}}))?;
        rendered(&response)
    }

    /// Round-trip a request through the whole pipeline.
    ///
    /// Unlike [`Client::ping`], which never reaches a module, this proves
    /// authentication, routing and dispatch work — what a readiness check
    /// actually wants. Needs the admin permission and the cluster module.
    pub fn admin_ping(&mut self) -> Result<()> {
        self.send(json!({"Admin": "Ping"})).map(|_| ())
    }

    /// The server's status, as the cluster core reports it.
    ///
    /// A single node without structured status answers with a plain message,
    /// which is returned under the `message` key rather than raised as an
    /// error.
    pub fn admin_status(&mut self) -> Result<Map<String, Value>> {
        let response = self.send(json!({"Admin": "Status"}))?;
        if response.kind == "Message" {
            let text = response.data.as_str().unwrap_or_default().to_string();
            let mut out = Map::new();
            out.insert("message".into(), Value::String(text));
            return Ok(out);
        }
        response.decode("admin status")
    }
}

/// Unwrap whichever payload the requested format produced.
fn rendered(response: &crate::Response) -> Result<String> {
    match response.kind.as_str() {
        "Toon" | "Message" => Ok(response
            .data
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| response.data.to_string())),
        "Json" => Ok(response.data.to_string()),
        other => Err(Error::protocol(format!(
            "expected a rendered export, got {}",
            if other.is_empty() { "no data" } else { other }
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DocumentFilter;

    #[test]
    fn formats_use_the_servers_spelling() {
        for (format, text) in [
            (OutputFormat::Native, "native"),
            (OutputFormat::Json, "json"),
            (OutputFormat::Toon, "toon"),
            (OutputFormat::Markdown, "markdown"),
        ] {
            assert_eq!(serde_json::to_value(format).unwrap(), text);
        }
    }

    #[test]
    fn the_default_options_redact() {
        let options = LlmOptions::default();
        assert!(options.redact_sensitive);
        assert_eq!(
            options.to_wire(),
            json!({"max_rows": null, "redact_sensitive": true, "include_schema": false})
        );
        assert_eq!(
            LlmOptions::default().max_rows(10).to_wire()["max_rows"],
            json!(10)
        );
    }

    #[test]
    fn sources_are_externally_tagged() {
        assert_eq!(
            serde_json::to_value(LlmSource::sql("SELECT 1")).unwrap(),
            json!({"Sql": {"query": "SELECT 1"}})
        );
        let source = LlmSource::document_find("products", &DocumentFilter::all(), None);
        let value = serde_json::to_value(source).unwrap();
        assert_eq!(value["DocumentFind"]["collection"], "products");
        assert_eq!(value["DocumentFind"]["filter"], json!("All"));
        assert_eq!(value["DocumentFind"]["limit"], Value::Null);
    }

    #[test]
    fn a_rendered_export_reads_text_or_json() {
        let mut response = crate::Response {
            request_id: "r".into(),
            status: "ok".into(),
            kind: "Toon".into(),
            data: json!("users:\n  1 ada"),
            route: None,
            elapsed_ms: None,
            warnings: vec![],
            error_code: None,
            leader_hint: None,
        };
        assert_eq!(rendered(&response).unwrap(), "users:\n  1 ada");
        response.kind = "Json".into();
        response.data = json!({"users": []});
        assert_eq!(rendered(&response).unwrap(), r#"{"users":[]}"#);
        response.kind = "Rows".into();
        assert!(rendered(&response).is_err());
    }
}
