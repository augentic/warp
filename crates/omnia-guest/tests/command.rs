//! Command façade contracts: argv parsing, the failure envelope, completions,
//! and the exit boundary.

#![cfg(all(not(target_arch = "wasm32"), feature = "command"))]

use clap::{Parser, Subcommand};
use omnia_guest::api::Format;
use omnia_guest::api::command::{
    Failure, IntoExit, Parsed, Response, Shell, USAGE_EXIT, completions, parse,
};
use omnia_guest::{bad_request, not_found};

#[derive(Debug, Parser)]
#[command(name = "app", bin_name = "app", version = "1.2.3", subcommand_required = true)]
struct App {
    #[arg(long, default_value = "text", global = true)]
    format: Format,

    #[command(subcommand)]
    verb: Verb,
}

#[derive(Debug, Subcommand)]
enum Verb {
    Greet { name: String },
    Completions { shell: Shell },
}

#[test]
fn parse_app() {
    let Parsed::App(app) = parse::<App>(["app", "--format", "json", "greet", "ada"]) else {
        panic!("argv parses into the grammar");
    };

    assert_eq!(app.format, Format::Json);
    assert!(matches!(app.verb, Verb::Greet { name } if name == "ada"));
}

#[test]
fn help_is_display_not_usage() {
    let Parsed::Display(text) = parse::<App>(["app", "--help"]) else {
        panic!("help is a display outcome");
    };

    assert!(text.contains("Usage: app"));
    assert!(text.contains("greet"));
}

#[test]
fn version_is_display() {
    let Parsed::Display(text) = parse::<App>(["app", "--version"]) else {
        panic!("version is a display outcome");
    };

    assert_eq!(text.trim(), "app 1.2.3");
}

#[test]
fn usage_error_exits_64() {
    let Parsed::Usage(error) = parse::<App>(["app", "bogus"]) else {
        panic!("an unknown verb is a usage error");
    };
    let response = Response::usage(&error);

    assert_eq!(response.exit, USAGE_EXIT);
    assert_eq!(response.exit, 64);
    assert!(response.stdout.is_empty());
    let stderr = String::from_utf8(response.stderr).expect("clap renders utf-8");
    assert!(stderr.contains("bogus"));
    assert!(stderr.contains("Usage: app"));
}

#[test]
fn failure_json_is_flat_kebab_envelope() {
    let plain = Failure::from(not_found!("no item 42"));
    assert_eq!(plain.exit_code(), 2);
    assert_eq!(plain.hint(), None);
    assert_eq!(
        serde_json::to_value(&plain).expect("envelope serializes"),
        serde_json::json!({ "error": "not_found", "message": "no item 42", "exit-code": 2 })
    );

    let hinted = Failure::from(bad_request!("name is required")).with_hint("pass --name");
    assert_eq!(hinted.body().error, "bad_request");
    assert_eq!(
        serde_json::to_value(&hinted).expect("envelope serializes"),
        serde_json::json!({
            "error": "bad_request",
            "message": "name is required",
            "exit-code": 1,
            "hint": "pass --name"
        })
    );
}

#[test]
fn failure_text_with_hint() {
    let failure = Failure::from(not_found!("no item 42")).with_hint("run `app list`");

    let mut text = String::new();
    failure.text(&mut text).expect("a String sink never fails");
    assert_eq!(text, "error[not_found]: no item 42\nhint: run `app list`\n");

    let encoded = Format::Text.encode(&failure, Failure::text);
    assert_eq!(encoded.bytes, text.into_bytes());
}

#[test]
fn failure_from_anyhow_classifies_through_error() {
    let foreign = Failure::from(anyhow::anyhow!("disk on fire"));
    assert_eq!(foreign.error().code(), "server_error");
    assert_eq!(foreign.exit_code(), 3);

    let domain = Failure::from(anyhow::Error::from(not_found!("gone")));
    assert_eq!(domain.exit_code(), 2);
}

#[test]
fn completions_bash_nonempty() {
    let response = completions::<App>(Shell::Bash, "app");

    assert_eq!(response.exit, 0);
    assert!(response.stderr.is_empty());
    let script = String::from_utf8(response.stdout).expect("completion script is utf-8");
    assert!(script.contains("_app"));
    assert!(script.contains("greet"));
}

#[test]
fn into_exit_passes_status() {
    assert_eq!(Response::success("").into_exit(), Ok(()));
    assert_eq!(Response::failure("", 2).into_exit(), Err(2));
    assert_eq!(Response::failure("", USAGE_EXIT).into_exit(), Err(64));
}
