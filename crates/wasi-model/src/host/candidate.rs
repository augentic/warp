//! The backend's answer and the candidate extraction it may apply to a
//! model's final text. Nothing here validates: acceptance is the guest's
//! `check`, or nothing at all when the request declares none.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::host::generated::omnia::model::completion::{Format, Usage as ReplyUsage};

/// A backend's result: the answer text, optional usage, and transcript.
///
/// Host-only — the guest sees a `reply` carrying `answer` and `usage`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer {
    /// The text the guest's check accepted, or the model's final text.
    pub answer: String,
    /// Token accounting the backend reported, surfaced to the guest as `reply.usage`.
    pub usage: Option<Usage>,
    /// Optional tool-call transcript the backend captured.
    pub transcript: Option<Transcript>,
}

/// Token accounting for one completion. Mirrors the WIT `usage` record; the
/// serde derive lets backends record it alongside the transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Prompt tokens consumed.
    pub input_tokens: u32,
    /// Completion tokens produced.
    pub output_tokens: u32,
    /// Reasoning tokens, for models that bill them separately.
    pub reasoning_tokens: Option<u32>,
}

/// The tool-call transcript a backend may capture for diagnostics or future
/// replay. Host-only; it never crosses the WIT boundary. Empty when the
/// backend captured no tool turns.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    /// Ordered tool turns the backend drove to reach the answer.
    pub turns: Vec<ToolTurn>,
}

/// One recorded tool interaction within a completion's transcript.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolTurn {
    /// The tool the model called.
    pub tool: String,
    /// The arguments the model supplied.
    pub args: serde_json::Value,
    /// The result the host returned.
    pub result: serde_json::Value,
}

impl From<Usage> for ReplyUsage {
    fn from(usage: Usage) -> Self {
        Self {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            reasoning_tokens: usage.reasoning_tokens,
        }
    }
}

impl From<String> for Answer {
    fn from(answer: String) -> Self {
        Self {
            answer,
            usage: None,
            transcript: None,
        }
    }
}

impl From<&str> for Answer {
    fn from(answer: &str) -> Self {
        answer.to_owned().into()
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Text => "text",
            Self::Json => "json",
            Self::Schema(_) => "schema",
        })
    }
}

impl Format {
    /// The final-answer instruction appended to a prompt for backends that
    /// steer output shape through prose rather than a provider `response_format`.
    #[must_use]
    pub fn instruction(&self) -> String {
        match self {
            Self::Schema(spec) => format!(
                "When you are done, reply with only your final answer as a single JSON value \
                 conforming to this JSON Schema, and nothing else:\n{}",
                spec.schema
            ),
            Self::Json => "When you are done, reply with only your final answer as a single JSON \
                           object and nothing else."
                .to_owned(),
            Self::Text => {
                "When you are done, reply with only your final answer as plain text and nothing \
                 else."
                    .to_owned()
            }
        }
    }

    /// The candidate answer in a model's final text: the text itself for
    /// `text`; for `json` and `schema`, the whole text when it parses as
    /// JSON, otherwise the last fenced or brace-delimited JSON value, and
    /// the raw text when there is none. A courtesy for providers that wrap
    /// JSON in prose, never a gate — the guest's check decides.
    #[must_use]
    pub fn candidate(&self, text: &str) -> String {
        match self {
            Self::Text => text.to_owned(),
            Self::Json | Self::Schema(_) => {
                maybe_json(text).last().map_or_else(|| text.to_owned(), ToString::to_string)
            }
        }
    }
}

// Every JSON value in `text`: the whole text alone when it parses, else the
// bodies of "```" fences and every `{` / `[` slice, in order.
fn maybe_json(text: &str) -> Vec<Value> {
    // try to parse the whole text as a single JSON value
    let text = text.trim();
    if let Ok(value) = serde_json::from_str(text) {
        return vec![value];
    }

    // extract values from "```" fences: fence bodies are the odd-indexed
    // chunks between the delimiters, minus their language-tag line
    let mut values = Vec::new();
    for body in text.split("```").skip(1).step_by(2) {
        let body = body.split_once('\n').map_or(body, |(_tag, body)| body);
        if let Ok(value) = serde_json::from_str(body.trim()) {
            values.push(value);
        }
    }

    // extract values from `{` or `[` slices
    let mut rest = text;
    while let Some(offset) = rest.find(['{', '[']) {
        let mut stream = serde_json::Deserializer::from_str(&rest[offset..]).into_iter::<Value>();
        match stream.next() {
            Some(Ok(value)) => {
                rest = &rest[offset + stream.byte_offset()..];
                values.push(value);
            }
            Some(Err(_)) | None => rest = &rest[offset + 1..],
        }
    }
    values
}

// Unit tests by design: `candidate` / `instruction` are the pure extraction
// and prompt-shaping surface backends drive directly; no guest boundary
// reaches them with these inputs.
#[cfg(test)]
mod tests {
    use super::Format;
    use crate::host::generated::omnia::model::completion::Schema;

    fn verdict_schema() -> Format {
        Format::Schema(Schema {
            name: "verdict".to_owned(),
            schema: r#"{"type":"object"}"#.to_owned(),
        })
    }

    #[test]
    fn text_passthrough() {
        assert_eq!(Format::Text.candidate("  plain {not: json}  "), "  plain {not: json}  ");
    }

    #[test]
    fn json_document() {
        assert_eq!(Format::Json.candidate(r#"{"verdict":"pass"}"#), r#"{"verdict":"pass"}"#);
    }

    #[test]
    fn fenced_json() {
        let fenced = "```json\n{\"verdict\":\"pass\"}\n```";
        assert_eq!(verdict_schema().candidate(fenced), r#"{"verdict":"pass"}"#);
    }

    #[test]
    fn json_with_preamble() {
        let text = "Done.\n{\"verdict\":\"pass\"}\n";
        assert_eq!(Format::Json.candidate(text), r#"{"verdict":"pass"}"#);
    }

    #[test]
    fn last_value_wins() {
        let text = "findings: []\n{\"outcome\":\"completed\"}";
        assert_eq!(verdict_schema().candidate(text), r#"{"outcome":"completed"}"#);
    }

    #[test]
    fn no_json() {
        assert_eq!(Format::Json.candidate("not json"), "not json");
    }

    #[test]
    fn instruction_per_format() {
        assert!(Format::Text.instruction().contains("plain text"));
        assert!(Format::Json.instruction().contains("JSON object"));
        let schema = verdict_schema().instruction();
        assert!(schema.contains("JSON Schema"), "unexpected: {schema}");
        assert!(schema.contains("object"), "unexpected: {schema}");
    }

    #[test]
    fn format_display() {
        assert_eq!(Format::Text.to_string(), "text");
        assert_eq!(Format::Json.to_string(), "json");
        assert_eq!(verdict_schema().to_string(), "schema");
    }
}
