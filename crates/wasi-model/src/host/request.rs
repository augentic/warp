//! Request validation and prompt shaping.

use std::fmt;

use crate::host::Error;
use crate::host::generated::omnia::model::completion::{Format, Mcp, Request, Role, Tool};

const RESERVED_TOOLS: &[&str] = &["read", "list", "write", "check"];

impl Request {
    /// The request's MCP server grants, each carrying its own endpoint URL.
    #[must_use]
    pub fn mcp_servers(&self) -> Vec<&Mcp> {
        self.tools
            .iter()
            .filter_map(|tool| match tool {
                Tool::Mcp(grant) => Some(grant),
                Tool::Function(_) => None,
            })
            .collect()
    }

    /// The request's tool names, each carrying its own parameters schema.
    pub fn tool_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter_map(|tool| match tool {
                Tool::Function(function) => Some(function.name.clone()),
                Tool::Mcp(_) => None,
            })
            .collect()
    }

    /// Validate the request and its tools.
    ///
    /// # Errors
    ///
    /// Returns an `InvalidRequest` error if the request is invalid.
    pub fn validate(&self) -> Result<(), Error> {
        for tool in &self.tools {
            let Tool::Function(function) = tool else {
                continue;
            };
            if RESERVED_TOOLS.contains(&function.name.as_str()) {
                return Err(Error::InvalidRequest(format!(
                    "reserved tool name: {}",
                    function.name
                )));
            }
            if serde_json::from_str::<serde_json::Value>(&function.parameters).is_err() {
                return Err(Error::InvalidRequest(format!(
                    "function tool `{}` parameters is not valid JSON",
                    function.name
                )));
            }
        }

        if self.messages.iter().all(|message| message.content.trim().is_empty()) {
            return Err(Error::InvalidRequest("empty request".to_owned()));
        }

        if let Format::Schema(spec) = &self.format
            && serde_json::from_str::<serde_json::Value>(&spec.schema).is_err()
        {
            return Err(Error::InvalidRequest("format schema is not valid JSON".to_owned()));
        }

        Ok(())
    }
}

impl fmt::Display for Request {
    // The prompt is the request's blocks joined by blank lines: the system
    // text, each message (non-user ones under a `[role]` header), and the
    // format's final-answer instruction.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut blocks: Vec<String> = self.system.iter().cloned().collect();
        blocks.extend(self.messages.iter().map(|message| match message.role {
            Role::User => message.content.clone(),
            Role::System | Role::Assistant => {
                format!("[{}]\n{}", message.role, message.content)
            }
        }));
        blocks.push(self.format.instruction());
        f.write_str(&blocks.join("\n\n"))
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Format, Mcp, Request, Role, Tool};
    use crate::host::{Function, Grants, Message};

    fn request(system: Option<&str>, messages: Vec<(Role, &str)>, tools: Vec<Tool>) -> Request {
        Request {
            model: None,
            system: system.map(str::to_owned),
            messages: messages
                .into_iter()
                .map(|(role, content)| Message {
                    role,
                    content: content.to_owned(),
                })
                .collect(),
            generation: None,
            format: Format::Text,
            tools,
            grants: Grants { workspace: None },
            check: false,
        }
    }

    #[test]
    fn mcp_servers_skip_functions() {
        let request = request(
            None,
            vec![(Role::User, "hi")],
            vec![
                Tool::Function(Function {
                    name: "lookup".to_owned(),
                    description: "lookup".to_owned(),
                    parameters: "{}".to_owned(),
                }),
                Tool::Mcp(Mcp {
                    name: "docs".to_owned(),
                    tools: vec!["search".to_owned()],
                    url: "https://mcp.example".to_owned(),
                }),
            ],
        );
        let names: Vec<&str> =
            request.mcp_servers().iter().map(|grant| grant.name.as_str()).collect();
        assert_eq!(names, ["docs"]);
        assert_eq!(request.tool_names(), ["lookup"]);
    }

    #[test]
    fn prompt_joins_channels() {
        let prompt = request(
            Some("be terse"),
            vec![(Role::User, "hi"), (Role::Assistant, "yo"), (Role::System, "note")],
            vec![],
        )
        .to_string();
        assert!(
            prompt.starts_with("be terse\n\nhi\n\n[assistant]\nyo\n\n[system]\nnote\n\n"),
            "{prompt}"
        );
        assert!(prompt.contains("plain text"), "{prompt}");
    }

    #[test]
    fn role_display() {
        assert_eq!(Role::System.to_string(), "system");
        assert_eq!(Role::User.to_string(), "user");
        assert_eq!(Role::Assistant.to_string(), "assistant");
    }
}
