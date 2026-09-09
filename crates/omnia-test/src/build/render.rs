//! Pure text rendering: `gen.rs`, the `[[example]]` list, and dep-info.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::Program;

/// One built component: the program plus its artifact path.
pub(super) struct Artifact<'a> {
    pub(super) program: &'a Program,
    pub(super) path: String,
}

/// Renders `gen.rs`: one `pub const <CONSTANT>: &str` per artifact, then one
/// `foreach_<group>!` completeness macro per group (an ungrouped artifact —
/// an extra package — gets a constant and no arm).
pub(super) fn render_gen(header: &str, artifacts: &[Artifact<'_>]) -> String {
    let mut generated = String::from(header);
    let mut groups: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for artifact in artifacts {
        let name = artifact.program.name.as_str();
        writeln!(
            generated,
            "/// Path to the compiled `{name}` guest component.\npub const {}: &str = {:?};",
            artifact.program.constant, artifact.path
        )
        .expect("writing to a String");
        if !artifact.program.group.is_empty() {
            groups.entry(&artifact.program.group).or_default().push(name);
        }
    }

    for (group, names) in groups {
        let expect = "        #[expect(unused_imports, reason = \"asserts the test exists\")]\n";
        let arms = names.iter().fold(String::new(), |mut arms, name| {
            let _ = writeln!(arms, "{expect}        use self::{name} as _;");
            arms
        });
        write!(
            generated,
            "/// Asserts an identically named test exists at the invocation site for \
             every `{group}` guest program; a program without one fails to \
             compile.\n#[macro_export]\nmacro_rules! \
             foreach_{group} {{\n    () => {{\n{arms}    }};\n}}\n"
        )
        .expect("writing to a String");
    }
    generated
}

/// Renders the manifest with everything after `marker` replaced by one
/// `[[example]]` stanza per program; `None` when the manifest lacks the marker.
pub(super) fn refresh_examples(
    current: &str, marker: &str, programs: &[Program],
) -> Option<String> {
    let (header, _) = current.split_once(marker)?;
    let mut next = String::from(header);
    next.push_str(marker);
    for program in programs {
        write!(
            next,
            "\n[[example]]\nname = \"{}\"\npath = \"{}\"\ncrate-type = [\"cdylib\"]\n",
            program.name, program.source
        )
        .expect("writing to a String");
    }
    Some(next)
}

/// The prerequisite paths in a cargo dep-info file, unescaping `\ `.
pub(super) fn dep_info_paths(contents: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in contents.lines() {
        let Some((_, deps)) = line.split_once(": ") else {
            continue;
        };
        let mut current = String::new();
        for part in deps.split(' ') {
            if let Some(prefix) = part.strip_suffix('\\') {
                current.push_str(prefix);
                current.push(' ');
            } else if !part.is_empty() {
                current.push_str(part);
                paths.push(std::mem::take(&mut current));
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(name: &str, group: &str, source: &str) -> Program {
        Program {
            name: name.into(),
            constant: name.to_uppercase(),
            group: group.into(),
            source: source.into(),
        }
    }

    // A package constant carries its group; an ungrouped extra has a
    // constant and no `foreach_` arm.
    #[test]
    fn gen_package_constants() {
        let intent = Program {
            name: "intent".into(),
            constant: "SOURCE_INTENT".into(),
            group: "source".into(),
            source: "intent".into(),
        };
        let caller = Program {
            name: "caller".into(),
            constant: "CALLER".into(),
            group: String::new(),
            source: "caller".into(),
        };
        let artifacts = [
            Artifact {
                program: &intent,
                path: "/out/intent.wasm".into(),
            },
            Artifact {
                program: &caller,
                path: "/out/caller.wasm".into(),
            },
        ];
        let rendered = render_gen("", &artifacts);
        assert!(rendered.contains("pub const SOURCE_INTENT: &str = \"/out/intent.wasm\";"));
        assert!(rendered.contains("pub const CALLER: &str = \"/out/caller.wasm\";"));
        assert!(rendered.contains("macro_rules! foreach_source"));
        assert!(rendered.contains("use self::intent as _;"));
        assert!(!rendered.contains("use self::caller as _;"));
        assert_eq!(rendered.matches("macro_rules!").count(), 1);
    }

    #[test]
    fn gen_macros() {
        let programs = [
            program("link_echo", "link", "programs/link/echo.rs"),
            program("model_echo_text", "model", "programs/model/echo_text.rs"),
            program("model_tools", "model", "programs/model/tools.rs"),
        ];
        let artifacts: Vec<_> = programs
            .iter()
            .map(|program| Artifact {
                program,
                path: format!("/out/{}.wasm", program.name),
            })
            .collect();

        let rendered = render_gen("// header\n", &artifacts);
        assert_eq!(
            rendered,
            "// header\n\
             /// Path to the compiled `link_echo` guest component.\n\
             pub const LINK_ECHO: &str = \"/out/link_echo.wasm\";\n\
             /// Path to the compiled `model_echo_text` guest component.\n\
             pub const MODEL_ECHO_TEXT: &str = \"/out/model_echo_text.wasm\";\n\
             /// Path to the compiled `model_tools` guest component.\n\
             pub const MODEL_TOOLS: &str = \"/out/model_tools.wasm\";\n\
             /// Asserts an identically named test exists at the invocation site for every \
             `link` guest program; a program without one fails to compile.\n\
             #[macro_export]\n\
             macro_rules! foreach_link {\n    () => {\n        \
             #[expect(unused_imports, reason = \"asserts the test exists\")]\n        \
             use self::link_echo as _;\n    };\n}\n\
             /// Asserts an identically named test exists at the invocation site for every \
             `model` guest program; a program without one fails to compile.\n\
             #[macro_export]\n\
             macro_rules! foreach_model {\n    () => {\n        \
             #[expect(unused_imports, reason = \"asserts the test exists\")]\n        \
             use self::model_echo_text as _;\n        \
             #[expect(unused_imports, reason = \"asserts the test exists\")]\n        \
             use self::model_tools as _;\n    };\n}\n"
        );
    }

    #[test]
    fn gen_quotes_paths() {
        let programs = [program("adapter", "examples", "examples/adapter/main.rs")];
        let artifacts = [Artifact {
            program: &programs[0],
            path: r"C:\out\adapter.wasm".into(),
        }];
        let rendered = render_gen("", &artifacts);
        assert!(rendered.contains(r#"pub const ADAPTER: &str = "C:\\out\\adapter.wasm";"#));
    }

    #[test]
    fn examples_replace() {
        let marker = "# Generated\n";
        let current = "[package]\nname = \"p\"\n\n# Generated\n\n[[example]]\nname = \"stale\"\n";
        let programs = [
            program("link_echo", "link", "programs/link/echo.rs"),
            program("model_tools", "model", "programs/model/tools.rs"),
        ];
        let next = refresh_examples(current, marker, &programs).expect("marker present");
        assert_eq!(
            next,
            "[package]\nname = \"p\"\n\n# Generated\n\n\
             [[example]]\nname = \"link_echo\"\npath = \"programs/link/echo.rs\"\n\
             crate-type = [\"cdylib\"]\n\n\
             [[example]]\nname = \"model_tools\"\npath = \"programs/model/tools.rs\"\n\
             crate-type = [\"cdylib\"]\n"
        );
    }

    #[test]
    fn examples_without_marker() {
        assert!(refresh_examples("[package]\n", "# Generated\n", &[]).is_none());
    }

    #[test]
    fn dep_info() {
        let contents = "/out/a.wasm: /src/lib.rs /my\\ dir/with\\ space.rs /src/b.rs\n\n\
                        /src/lib.rs:\n/src/b.rs:\n";
        assert_eq!(dep_info_paths(contents), ["/src/lib.rs", "/my dir/with space.rs", "/src/b.rs"]);
    }
}
