//! # CLI Command Wasm Guest
//!
//! A `wasi:cli/command` reactor: clap argv, `Client::call` on fn handlers,
//! and `command::execute_wasi`.
//!
//! The module is `#[cfg(target_arch = "wasm32")]`-guarded because examples
//! also compile for the host triple, where `wasip3` is unavailable.

#![cfg(target_arch = "wasm32")]

use std::error::Error;
use std::fmt;

use clap::{Args, Parser};
use omnia_guest::api::{Client, Context, Metadata, command};
use wasip3::exports::cli::run::Guest;

#[derive(Parser)]
#[command(
    name = "cli",
    bin_name = "cli",
    version = env!("CARGO_PKG_VERSION"),
    about = "Omnia wasi:cli/command example",
    arg_required_else_help = true,
    subcommand_required = true
)]
enum App {
    /// Print a greeting
    Greet(GreetInput),
    /// Print the sum of the integer arguments
    Add(AddInput),
    /// Print the inherited environment, one key=value per line
    Env,
    /// Exit with CODE via wasi:cli/exit, or fail plainly (exit 1) without it
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
    /// Specific exit code to carry through wasi:cli/exit
    code: Option<u8>,
}

struct Provider {
    greeting: &'static str,
}

#[derive(Debug)]
enum CommandError {
    Exit(u8),
    Plain,
}

impl fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exit(_) => Ok(()),
            Self::Plain => formatter.write_str("failing plainly\n"),
        }
    }
}

impl Error for CommandError {}

async fn greet(input: GreetInput, context: Context<Provider>) -> Result<String, CommandError> {
    Ok(format!("{}, {}!\n", context.provider().greeting, input.name))
}

async fn add(input: AddInput, _context: Context<Provider>) -> Result<String, CommandError> {
    Ok(format!("{}\n", input.numbers.iter().sum::<i64>()))
}

async fn env((): (), _context: Context<Provider>) -> Result<String, CommandError> {
    Ok(std::env::vars().map(|(key, value)| format!("{key}={value}\n")).collect())
}

async fn fail(input: FailInput, _context: Context<Provider>) -> Result<String, CommandError> {
    Err(input.code.map_or(CommandError::Plain, CommandError::Exit))
}

struct Cli;
wasip3::cli::command::export!(Cli);

impl Guest for Cli {
    async fn run() -> Result<(), ()> {
        command::execute_wasi(dispatch()).await
    }
}

async fn dispatch() -> Result<(), u8> {
    let app = App::try_parse_from(wasip3::cli::environment::get_arguments()).map_err(|error| {
        let _ = error.print();
        u8::try_from(error.exit_code()).unwrap_or(2)
    })?;
    let client = Client::new("examples", Provider { greeting: "Hello" });
    let metadata = Metadata::default();
    let result = match app {
        App::Greet(input) => client.call(greet, input, &metadata).await,
        App::Add(input) => client.call(add, input, &metadata).await,
        App::Env => client.call(env, (), &metadata).await,
        App::Fail(input) => client.call(fail, input, &metadata).await,
    };
    match result {
        Ok(output) => {
            print!("{output}");
            Ok(())
        }
        Err(error) => {
            eprint!("{error}");
            Err(match error {
                CommandError::Exit(code) => code,
                CommandError::Plain => 1,
            })
        }
    }
}
