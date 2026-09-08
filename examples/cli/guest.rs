//! # CLI Command Wasm Guest
//!
//! A `wasi:cli/command` reactor on the command façade: `parse` classifies
//! argv into the clap grammar, `Command::call` projects each verb (decode →
//! `Client::call` → encode) onto a `Response`, and `command!` binds `main` as
//! the `wasi:cli/run` export.
//!
//! The module is `#[cfg(target_arch = "wasm32")]`-guarded because examples
//! also compile for the host triple, where `wasip3` is unavailable.

#![cfg(target_arch = "wasm32")]

use std::collections::BTreeMap;
use std::fmt;

use clap::{Args, Parser, Subcommand, ValueEnum};
use omnia_guest::api::command::{Command, Parsed, Response, parse};
use omnia_guest::api::{Client, Context, Format, Metadata};
use omnia_guest::{Error, bad_gateway, bad_request, not_found, server_error};
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "cli",
    bin_name = "cli",
    version = env!("CARGO_PKG_VERSION"),
    about = "Omnia wasi:cli/command example",
    arg_required_else_help = true,
    subcommand_required = true
)]
struct App {
    /// Output format for every verb
    #[arg(long, global = true, default_value = "text")]
    format: Format,

    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    /// Print a greeting
    Greet(GreetInput),
    /// Print the sum of the integer arguments
    Add(AddInput),
    /// Print the inherited environment, one key=value per line
    Env,
    /// Fail with an error class; its exit code follows the exit map
    Fail(FailInput),
}

#[derive(Args)]
struct GreetInput {
    /// Who to greet
    #[arg(default_value = "world")]
    name: String,
}

#[derive(Args)]
struct AddInput {
    /// Integers to sum
    numbers: Vec<i64>,
}

#[derive(Args)]
struct FailInput {
    /// The error class to fail with
    #[arg(default_value = "server-error")]
    class: ErrorClass,
}

#[derive(Clone, Copy, ValueEnum)]
enum ErrorClass {
    /// Exit 1
    BadRequest,
    /// Exit 2
    NotFound,
    /// Exit 3
    ServerError,
    /// Exit 4
    BadGateway,
}

struct Provider {
    greeting: &'static str,
}

#[derive(Serialize)]
struct Greeting {
    greeting: String,
}

#[derive(Serialize)]
struct Sum {
    sum: i64,
}

async fn greet(input: GreetInput, context: Context<Provider>) -> Result<Greeting, Error> {
    Ok(Greeting {
        greeting: format!("{}, {}!", context.provider().greeting, input.name),
    })
}

async fn add(input: AddInput, _context: Context<Provider>) -> Result<Sum, Error> {
    Ok(Sum {
        sum: input.numbers.iter().sum(),
    })
}

async fn env((): (), _context: Context<Provider>) -> Result<BTreeMap<String, String>, Error> {
    Ok(std::env::vars().collect())
}

async fn fail(input: FailInput, _context: Context<Provider>) -> Result<(), Error> {
    Err(match input.class {
        ErrorClass::BadRequest => bad_request!("failing as a bad request"),
        ErrorClass::NotFound => not_found!("failing as not found"),
        ErrorClass::ServerError => server_error!("failing as a server error"),
        ErrorClass::BadGateway => bad_gateway!("failing as a bad gateway"),
    })
}

fn render_greeting(greeting: &Greeting, out: &mut dyn fmt::Write) -> fmt::Result {
    writeln!(out, "{}", greeting.greeting)
}

fn render_sum(sum: &Sum, out: &mut dyn fmt::Write) -> fmt::Result {
    writeln!(out, "{}", sum.sum)
}

fn render_env(vars: &BTreeMap<String, String>, out: &mut dyn fmt::Write) -> fmt::Result {
    vars.iter().try_for_each(|(key, value)| writeln!(out, "{key}={value}"))
}

fn render_nothing((): &(), _out: &mut dyn fmt::Write) -> fmt::Result {
    Ok(())
}

omnia_guest::command!(main);

async fn main() -> Response {
    match parse::<App>(wasip3::cli::environment::get_arguments()) {
        Parsed::App(app) => dispatch(app).await,
        Parsed::Display(text) => Response::success(text),
        Parsed::Usage(error) => Response::usage(&error),
    }
}

async fn dispatch(app: App) -> Response {
    let client = Client::new("examples", Provider { greeting: "Hello" });
    let metadata = Metadata::from_env("CLI");
    let command = Command::new(&client, &metadata, app.format);
    match app.verb {
        Verb::Greet(input) => command.call(greet, || Ok(input), render_greeting).await,
        Verb::Add(input) => command.call(add, || Ok(input), render_sum).await,
        Verb::Env => command.call(env, || Ok(()), render_env).await,
        Verb::Fail(input) => command.call(fail, || Ok(input), render_nothing).await,
    }
}
