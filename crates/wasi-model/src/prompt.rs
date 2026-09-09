//! Structured prompt template assembled into the request's `system` /
//! `messages` channels.
//!
//! The boundary itself is plain `system` + `messages` (like provider APIs);
//! this builder turns a role/task/context template into those channels
//! before `create` is called.

#[cfg(target_arch = "wasm32")]
use crate::completion::{Message, Role};

/// Structured prompt template assembled into `system` / `messages` channels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sections {
    /// Persona / actor instruction; joins the system channel.
    pub role: Option<String>,
    /// What the model should do. Required.
    pub task: String,
    /// Background documents, prior state, or prior turns.
    pub context: Option<String>,
    /// Rules, limits, and things to avoid; formatted as system-channel bullets.
    pub constraints: Vec<String>,
    /// Few-shot input/output pairs appended to the user turn.
    pub examples: Vec<Example>,
    /// `{name}` placeholders substituted into every section text.
    pub variables: Vec<(String, String)>,
}

/// One few-shot input/output pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Example {
    /// Example user input.
    pub input: String,
    /// Example model output.
    pub output: String,
}

impl Sections {
    /// Assemble the template into `(system, user-turn)` text: the system
    /// channel is `preamble` + role + constraint bullets; the user turn is
    /// task + context + formatted examples, with `{name}` variables
    /// substituted throughout.
    #[must_use]
    pub fn assemble(&self, preamble: Option<&str>) -> (Option<String>, String) {
        let substitute = |text: &str| {
            let mut out = text.to_owned();
            for (name, value) in &self.variables {
                out = out.replace(&format!("{{{name}}}"), value);
            }
            out
        };

        let mut system_parts: Vec<String> = Vec::new();
        if let Some(preamble) = preamble {
            system_parts.push(preamble.to_owned());
        }
        if let Some(role) = &self.role {
            system_parts.push(substitute(role));
        }
        for constraint in &self.constraints {
            system_parts.push(format!("- {}", substitute(constraint)));
        }

        let mut user_parts: Vec<String> = vec![substitute(&self.task)];
        if let Some(context) = &self.context {
            user_parts.push(substitute(context));
        }
        for example in &self.examples {
            user_parts.push(format!(
                "Input: {}\nOutput: {}",
                substitute(&example.input),
                substitute(&example.output)
            ));
        }

        (join(&system_parts), join(&user_parts).unwrap_or_default())
    }

    /// Assemble the template into the request's chat channels: the system
    /// string (led by `preamble` when given) and a single user turn.
    #[cfg(target_arch = "wasm32")]
    #[must_use]
    pub fn channels(&self, preamble: Option<&str>) -> (Option<String>, Vec<Message>) {
        let (system, user) = self.assemble(preamble);
        (
            system,
            vec![Message {
                role: Role::User,
                content: user,
            }],
        )
    }
}

fn join(parts: &[String]) -> Option<String> {
    let kept: Vec<&str> = parts.iter().map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    if kept.is_empty() { None } else { Some(kept.join("\n\n")) }
}

#[cfg(test)]
mod tests {
    use super::{Example, Sections};

    #[test]
    fn assemble_sections() {
        let sections = Sections {
            role: Some("a {language} reviewer".to_owned()),
            task: "review the {language} code".to_owned(),
            context: Some("the {language} crate".to_owned()),
            constraints: vec!["be {language}-idiomatic".to_owned()],
            examples: vec![Example {
                input: "in".to_owned(),
                output: "out".to_owned(),
            }],
            variables: vec![("language".to_owned(), "Rust".to_owned())],
        };
        let (system, user) = sections.assemble(Some("prefer {language}"));
        assert_eq!(
            system.as_deref(),
            Some("prefer {language}\n\na Rust reviewer\n\n- be Rust-idiomatic")
        );
        assert_eq!(user, "review the Rust code\n\nthe Rust crate\n\nInput: in\nOutput: out");
    }

    #[test]
    fn assemble_drops_blank_system() {
        let sections = Sections {
            role: Some("   ".to_owned()),
            task: "do it".to_owned(),
            context: Some(String::new()),
            ..Sections::default()
        };
        let (system, user) = sections.assemble(None);
        assert!(system.is_none());
        assert_eq!(user, "do it");
    }
}
