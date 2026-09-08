//! The exit map through the real runtime: each verb's error class leaves the
//! `command!` boundary as its mapped exit status (`bad` 1, `missing` 2,
//! `upstream` 4, `ok` 0), and an unknown verb is a clap usage error at
//! `USAGE_EXIT`.

#![cfg(target_arch = "wasm32")]

use std::fmt;

use clap::{Parser, Subcommand};
use omnia_guest::api::command::{Command, Parsed, Response, parse};
use omnia_guest::api::{Client, Context, Format, Metadata};
use omnia_guest::{Error, bad_gateway, bad_request, not_found};

#[derive(Parser)]
#[command(name = "exit-map", bin_name = "exit-map", subcommand_required = true)]
struct App {
    #[arg(long, global = true, default_value = "text")]
    format: Format,

    #[command(subcommand)]
    verb: Verb,
}

#[derive(Subcommand)]
enum Verb {
    Ok,
    Bad,
    Missing,
    Upstream,
}

struct Provider;

async fn ok((): (), _context: Context<Provider>) -> Result<String, Error> {
    Ok("ok".to_owned())
}

async fn bad((): (), _context: Context<Provider>) -> Result<(), Error> {
    Err(bad_request!("refused as a bad request"))
}

async fn missing((): (), _context: Context<Provider>) -> Result<(), Error> {
    Err(not_found!("refused as not found"))
}

async fn upstream((): (), _context: Context<Provider>) -> Result<(), Error> {
    Err(bad_gateway!("refused as a bad gateway"))
}

fn render_text(text: &String, out: &mut dyn fmt::Write) -> fmt::Result {
    writeln!(out, "{text}")
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
    let client = Client::new("exit-map", Provider);
    let metadata = Metadata::from_env("EXIT_MAP");
    let command = Command::new(&client, &metadata, app.format);
    match app.verb {
        Verb::Ok => command.call(ok, || Ok(()), render_text).await,
        Verb::Bad => command.call(bad, || Ok(()), render_nothing).await,
        Verb::Missing => command.call(missing, || Ok(()), render_nothing).await,
        Verb::Upstream => command.call(upstream, || Ok(()), render_nothing).await,
    }
}
