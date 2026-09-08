//! Smoke-test every example against its README contract: start each host
//! binary, drive the documented requests/argv, and assert status codes and
//! exit codes.
//!
//! IMPORTANT: Assumes guests and hosts are already built — run via
//! `cargo make smoke`, which builds both first. The test is `#[ignore]`d so
//! the regular `cargo make test` loop skips it. `SMOKE_FILTER` narrows a run
//! to scenarios whose names start with any of its comma-separated prefixes,
//! e.g. `SMOKE_FILTER=identity,cli/`.
//!
//! Credentialed backends are stood in for by in-process stubs (see
//! [`serve_token_stub`]), so a run needs no secrets. Outbound internet is
//! assumed: the http-proxy checks reach its public origin, and an unreachable
//! origin is a failure like any other.

use std::fs::{self, File};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::header::{AUTHORIZATION, CONTENT_TYPE};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use reqwest::Client;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const HELLO: &str = r#"{"text":"hello"}"#;

/// Credentials handed to the identity example; the token stub only honours a
/// client-credentials grant that authenticates with exactly these.
const IDENTITY_CLIENT_ID: &str = "smoke-client";
const IDENTITY_CLIENT_SECRET: &str = "smoke-secret";
/// `base64("smoke-client:smoke-secret")`, as RFC 6749 §2.3.1 HTTP Basic auth.
const IDENTITY_BASIC_AUTH: &str = "Basic c21va2UtY2xpZW50OnNtb2tlLXNlY3JldA==";

enum Scenario {
    /// Spawn `<bin> run [wasm]`, wait for port 8080, drive HTTP checks, stop.
    Server {
        name: &'static str,
        wasm: Option<&'static str>,
        checks: &'static [Check],
        /// Extra environment for the host process, resolved against the
        /// runtime context so it can point at in-process stubs.
        env: fn(&Ctx) -> Vec<(&'static str, String)>,
        /// Case-insensitive needle the host log must contain once the checks
        /// have run.
        expect_log: Option<&'static str>,
    },
    /// Run `<bin> [run <wasm> --] <args>` to completion and assert its exit.
    Command {
        name: &'static str,
        bin: &'static str,
        wasm: Option<&'static str>,
        args: &'static [&'static str],
        expect: Expect,
    },
    /// A flow with bespoke semantics.
    Custom(&'static str, fn(&Ctx) -> Vec<Outcome>),
}

impl Scenario {
    const fn name(&self) -> &'static str {
        match self {
            Self::Server { name, .. } | Self::Command { name, .. } | Self::Custom(name, _) => name,
        }
    }

    const fn server(
        name: &'static str, wasm: Option<&'static str>, checks: &'static [Check],
    ) -> Self {
        Self::Server {
            name,
            wasm,
            checks,
            env: no_env,
            expect_log: None,
        }
    }

    const fn cmd(
        name: &'static str, bin: &'static str, wasm: Option<&'static str>,
        args: &'static [&'static str], expect: Expect,
    ) -> Self {
        Self::Command {
            name,
            bin,
            wasm,
            args,
            expect,
        }
    }
}

struct Check {
    label: &'static str,
    // Held as a string so `Check` has no destructor, letting the check
    // arrays const-promote through the `Scenario::server` call.
    method: &'static str,
    path: &'static str,
    json_body: Option<&'static str>,
    expect_status: u16,
    expect_body_contains: Option<&'static str>,
}

impl Check {
    const fn req(
        method: &'static str, label: &'static str, path: &'static str,
        json_body: Option<&'static str>,
    ) -> Self {
        Self {
            label,
            method,
            path,
            json_body,
            expect_status: 200,
            expect_body_contains: None,
        }
    }

    const fn get(label: &'static str, path: &'static str) -> Self {
        Self::req("GET", label, path, None)
    }

    const fn post(label: &'static str, path: &'static str, body: &'static str) -> Self {
        Self::req("POST", label, path, Some(body))
    }

    const fn status(mut self, status: u16) -> Self {
        self.expect_status = status;
        self
    }

    const fn body_contains(mut self, needle: &'static str) -> Self {
        self.expect_body_contains = Some(needle);
        self
    }
}

enum Expect {
    /// Exit 0.
    Ok,
    /// Exit 0 with a case-insensitive needle in the output log.
    OkWith(&'static str),
    ExitCode(i32),
}

enum Outcome {
    Pass(String),
    Fail(String),
    Warn(String),
}

impl Outcome {
    fn line(&self) -> String {
        match self {
            Self::Pass(msg) => format!("PASS {msg}"),
            Self::Fail(msg) => format!("FAIL {msg}"),
            Self::Warn(msg) => format!("WARN {msg}"),
        }
    }
}

const SCENARIOS: &[Scenario] = &[
    Scenario::server(
        "http",
        Some("http_wasm.wasm"),
        &[Check::post("post", "/", HELLO), Check::get("get", "/")],
    ),
    Scenario::server("keyvalue", Some("keyvalue_wasm.wasm"), &[Check::post("post", "/", HELLO)]),
    Scenario::server("blobstore", Some("blobstore_wasm.wasm"), &[Check::post("post", "/", HELLO)]),
    Scenario::server("vault", Some("vault_wasm.wasm"), &[Check::post("post", "/", HELLO)]),
    Scenario::server(
        "sql",
        Some("sql_wasm.wasm"),
        &[
            Check::post(
                "create-agency",
                "/agencies",
                r#"{"agency_id":1,"name":"Ritchies Transport","url":"https://ritchies.co.nz","timezone":"Pacific/Auckland"}"#,
            ),
            Check::get("list-agencies", "/agencies"),
            Check::req(
                "PATCH",
                "patch-agency",
                "/agencies/1",
                Some(r#"{"name":"Ritchies Transport Agency","timezone":"Pacific/Auckland"}"#),
            ),
            Check::post(
                "create-feed",
                "/agencies/1/feeds",
                r#"{"feed_id":1,"description":"Bus routes and schedules"}"#,
            ),
            Check::get("list-feeds", "/feeds"),
            Check::req("DELETE", "delete-feed", "/feeds/1", None),
        ],
    ),
    Scenario::server(
        "docstore",
        Some("docstore_wasm.wasm"),
        &[
            Check::post(
                "create1",
                "/stops",
                r#"{"id":"stop-001","stop_name":"Britomart Transport Centre","stop_lat":-36.8442,"stop_lon":174.7676,"zone_id":"zone-1"}"#,
            ),
            Check::post(
                "create2",
                "/stops",
                r#"{"id":"stop-002","stop_name":"Newmarket Station","stop_lat":-36.8690,"stop_lon":174.7779,"zone_id":"zone-1"}"#,
            ),
            Check::post(
                "create3",
                "/stops",
                r#"{"id":"stop-003","stop_name":"Albany Station","stop_lat":-36.7275,"stop_lon":174.6986,"zone_id":"zone-3"}"#,
            ),
            Check::get("get", "/stops/stop-001"),
            Check::req(
                "PUT",
                "put",
                "/stops/stop-001",
                Some(
                    r#"{"stop_name":"Britomart","stop_lat":-36.8442,"stop_lon":174.7676,"zone_id":"zone-1"}"#,
                ),
            ),
            Check::get("query-all", "/stops"),
            Check::get("query-text", "/stops?q=Station"),
            Check::get("query-zone", "/stops?zone=zone-1"),
            Check::get("query-lat", "/stops?min_lat=-36.90&max_lat=-36.80"),
            Check::get("query-limit", "/stops?limit=2"),
            Check::req("DELETE", "delete", "/stops/stop-003", None),
        ],
    ),
    Scenario::server("otel", Some("otel_wasm.wasm"), &[Check::post("post", "/", HELLO)]),
    // All three routes proxy jsonplaceholder.cypress.io.
    Scenario::server(
        "http-proxy",
        Some("http_proxy_wasm.wasm"),
        &[
            Check::get("cache1", "/cache"),
            Check::get("cache2", "/cache"),
            Check::get("origin-sm", "/origin-sm"),
        ],
    ),
    Scenario::server(
        "messaging",
        Some("messaging_wasm.wasm"),
        &[Check::post("pub-sub", "/pub-sub", HELLO)],
    ),
    Scenario::server("config", Some("config_wasm.wasm"), &[Check::get("get", "/")]),
    Scenario::server("websocket", Some("websocket_wasm.wasm"), &[Check::post("post", "/", HELLO)]),
    Scenario::server(
        "mcp",
        Some("mcp_wasm.wasm"),
        &[
            Check::post(
                "tools-list",
                "/mcp/docs",
                r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#,
            ),
            Check::post(
                "tools-call",
                "/mcp/docs",
                r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"read_doc","arguments":{"name":"overview"}}}"#,
            ),
        ],
    ),
    // Manifest compiled in, so `run` takes no wasm argument.
    Scenario::server(
        "http-routing",
        None,
        &[
            Check::get("route-a", "/a").body_contains("guest a"),
            Check::get("route-b", "/b"),
            Check::get("route-c-404", "/c").status(404),
        ],
    ),
    Scenario::cmd("model/run", "model", None, &[], Expect::Ok),
    Scenario::cmd(
        "cli/greet",
        "cli",
        Some("cli_wasm.wasm"),
        &["greet", "Ada"],
        Expect::OkWith("ada"),
    ),
    Scenario::cmd(
        "cli/greet-json",
        "cli",
        Some("cli_wasm.wasm"),
        &["--format", "json", "greet", "Ada"],
        Expect::OkWith("\"greeting\""),
    ),
    Scenario::cmd(
        "cli/fail-not-found",
        "cli",
        Some("cli_wasm.wasm"),
        &["fail", "not-found"],
        Expect::ExitCode(2),
    ),
    Scenario::cmd("cli/bogus", "cli", Some("cli_wasm.wasm"), &["bogus"], Expect::ExitCode(64)),
    Scenario::cmd("cli/fail", "cli", Some("cli_wasm.wasm"), &["fail"], Expect::ExitCode(3)),
    // cli-static has no `run` grammar; argv is the command itself.
    Scenario::cmd("cli-static/greet", "cli-static", None, &["greet", "Ada"], Expect::OkWith("ada")),
    Scenario::cmd("cli-static/add", "cli-static", None, &["add", "2", "40"], Expect::Ok),
    Scenario::cmd(
        "cli-static/fail-not-found",
        "cli-static",
        None,
        &["fail", "not-found"],
        Expect::ExitCode(2),
    ),
    Scenario::Custom("guest-link", run_guest_link),
    Scenario::cmd("guest-link-dynamic", "guest-link-dynamic", None, &[], Expect::Ok),
    Scenario::cmd("guest-link-register", "guest-link-register", None, &[], Expect::Ok),
    Scenario::Custom("identity/fail-fast", run_identity_fail_fast),
    Scenario::Server {
        name: "identity",
        wasm: Some("identity_wasm.wasm"),
        checks: &[Check::get("get", "/").body_contains("Hello, World!")],
        env: identity_env,
        // Printed by the guest once the stub has minted a token.
        expect_log: Some("access token acquired"),
    },
];

struct Ctx {
    bin: PathBuf,
    wasm: PathBuf,
    log: PathBuf,
    rust_log: String,
    /// In-process OAuth2 token endpoint the identity example is pointed at.
    token_url: String,
}

fn no_env(_: &Ctx) -> Vec<(&'static str, String)> {
    Vec::new()
}

fn identity_env(ctx: &Ctx) -> Vec<(&'static str, String)> {
    vec![
        ("IDENTITY_CLIENT_ID", IDENTITY_CLIENT_ID.into()),
        ("IDENTITY_CLIENT_SECRET", IDENTITY_CLIENT_SECRET.into()),
        ("IDENTITY_TOKEN_URL", ctx.token_url.clone()),
    ]
}

#[tokio::test]
#[ignore = "needs pre-built example hosts and guests plus outbound internet; run via `cargo make smoke`"]
async fn examples() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().context("workspace root")?;
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let ctx = Ctx {
        bin: target.join("debug/examples"),
        wasm: target.join("wasm32-wasip2/debug/examples"),
        log: std::env::temp_dir().join(format!("omnia-smoke-{}", std::process::id())),
        rust_log: std::env::var("RUST_LOG").unwrap_or_else(|_| "info,opentelemetry_sdk=off".into()),
        token_url: serve_token_stub().await?,
    };
    fs::create_dir_all(&ctx.log)?;

    if port_open() {
        bail!("port 8080 is already in use; stop that server first");
    }

    let filters: Vec<String> = std::env::var("SMOKE_FILTER")
        .map(|v| v.split(',').filter(|f| !f.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default();
    let selected = SCENARIOS
        .iter()
        .filter(|s| filters.is_empty() || filters.iter().any(|f| s.name().starts_with(f.as_str())));

    // reqwest's TLS stack (`rustls-no-provider`) builds no client until a
    // process crypto provider is installed; the runtime does this inside
    // the host, but the smoke client lives in the test process.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    // Every server scenario restarts a host on the same port; a pooled
    // keep-alive socket to the previous host would be reused for the next
    // one's first request and get reset. Never keep idle connections.
    let client = Client::builder().timeout(REQUEST_TIMEOUT).pool_max_idle_per_host(0).build()?;
    let mut results = Vec::new();
    for scenario in selected {
        let outcomes = match scenario {
            Scenario::Server {
                name,
                wasm,
                checks,
                env,
                expect_log,
            } => run_server(&ctx, &client, name, *wasm, checks, env(&ctx), *expect_log).await,
            Scenario::Command {
                name,
                bin,
                wasm,
                args,
                expect,
            } => run_command(&ctx, name, bin, *wasm, args, expect),
            Scenario::Custom(_, run) => run(&ctx),
        };
        for outcome in outcomes {
            println!("{}", outcome.line());
            results.push(outcome);
        }
    }

    let pass = results.iter().filter(|o| matches!(o, Outcome::Pass(_))).count();
    let fail = results.iter().filter(|o| matches!(o, Outcome::Fail(_))).count();
    println!();
    println!("===== SUMMARY =====");
    println!("pass: {pass}");
    println!("fail: {fail}");
    for outcome in &results {
        if !matches!(outcome, Outcome::Pass(_)) {
            println!("{}", outcome.line());
        }
    }
    println!("logs: {}", ctx.log.display());
    if fail > 0 {
        bail!("{fail} smoke check(s) failed");
    }
    Ok(())
}

fn spawn_logged(
    ctx: &Ctx, bin: &str, log_path: &Path, args: &[&str], env: &[(&str, String)],
) -> Child {
    let log_file = File::create(log_path).expect("create log file");
    Command::new(ctx.bin.join(bin))
        .args(args)
        .env("RUST_LOG", &ctx.rust_log)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdout(Stdio::from(log_file.try_clone().expect("clone log file")))
        .stderr(Stdio::from(log_file))
        .spawn()
        .unwrap_or_else(|err| panic!("spawning {bin}: {err}"))
}

async fn run_server(
    ctx: &Ctx, client: &Client, name: &str, wasm: Option<&str>, checks: &[Check],
    env: Vec<(&str, String)>, expect_log: Option<&str>,
) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    let mut args = vec!["run".to_string()];
    if let Some(wasm) = wasm {
        args.push(ctx.wasm.join(wasm).display().to_string());
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let log_path = ctx.log.join(format!("{name}.log"));
    let mut child = spawn_logged(ctx, name, &log_path, &args, &env);

    let mut started = false;
    for _ in 0..120 {
        if port_open() {
            started = true;
            break;
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            outcomes.push(Outcome::Fail(format!("{name}/startup (process died)")));
            stop_server(&mut child, &mut outcomes);
            return outcomes;
        }
        sleep(Duration::from_millis(500));
    }
    if !started {
        outcomes.push(Outcome::Fail(format!("{name}/startup (port 8080 never opened)")));
        stop_server(&mut child, &mut outcomes);
        return outcomes;
    }

    for check in checks {
        run_check(client, name, check, &mut outcomes).await;
    }
    stop_server(&mut child, &mut outcomes);
    if let Some(needle) = expect_log {
        let log = fs::read_to_string(&log_path).unwrap_or_default();
        if log.to_lowercase().contains(&needle.to_lowercase()) {
            outcomes.push(Outcome::Pass(format!("{name}/log ({needle:?})")));
        } else {
            outcomes.push(Outcome::Fail(format!("{name}/log ({needle:?} not found)")));
        }
    }
    outcomes
}

async fn run_check(client: &Client, name: &str, check: &Check, outcomes: &mut Vec<Outcome>) {
    let label = check.label;
    match request(client, check).await {
        Ok((status, body)) => {
            if status == check.expect_status {
                outcomes.push(Outcome::Pass(format!("{name}/{label} ({status})")));
            } else {
                outcomes.push(Outcome::Fail(format!(
                    "{name}/{label} (got {status} want {}) body={}",
                    check.expect_status,
                    snippet(&body)
                )));
            }
            if let Some(needle) = check.expect_body_contains {
                if body.contains(needle) {
                    outcomes.push(Outcome::Pass(format!("{name}/{label}-body")));
                } else {
                    outcomes.push(Outcome::Fail(format!(
                        "{name}/{label}-body body={}",
                        snippet(&body)
                    )));
                }
            }
        }
        Err(err) => {
            outcomes.push(Outcome::Fail(format!("{name}/{label} (request failed: {err:#})")))
        }
    }
}

/// The client's timeout covers the whole exchange, so a hung server surfaces as
/// a failed check rather than stalling the run.
async fn request(client: &Client, check: &Check) -> Result<(u16, String)> {
    let method = reqwest::Method::from_bytes(check.method.as_bytes())?;
    let mut builder = client.request(method, format!("http://localhost:8080{}", check.path));
    if let Some(json) = check.json_body {
        builder = builder.header(CONTENT_TYPE, "application/json").body(json);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    let body = response.text().await?;
    Ok((status, body))
}

fn run_command(
    ctx: &Ctx, name: &str, bin: &str, wasm: Option<&str>, args: &[&str], expect: &Expect,
) -> Vec<Outcome> {
    let log_path = ctx.log.join(format!("{}.log", name.replace('/', "-")));
    let wasm_path = wasm.map(|wasm| ctx.wasm.join(wasm).display().to_string());
    let mut argv = Vec::new();
    if let Some(wasm_path) = &wasm_path {
        argv.extend(["run", wasm_path, "--"]);
    }
    argv.extend(args);
    let mut child = spawn_logged(ctx, bin, &log_path, &argv, &[]);
    let status = child.wait().expect("wait for child");
    let code = status.code().unwrap_or(-1);
    let outcome = match expect {
        Expect::Ok => {
            if code == 0 {
                Outcome::Pass(format!("{name} (exit 0)"))
            } else {
                Outcome::Fail(format!("{name} (exit {code})"))
            }
        }
        Expect::OkWith(needle) => {
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            if code == 0 && log.to_lowercase().contains(&needle.to_lowercase()) {
                Outcome::Pass(name.to_string())
            } else {
                Outcome::Fail(format!("{name} (exit {code}) {}", snippet(&log)))
            }
        }
        Expect::ExitCode(want) => {
            if code == *want {
                Outcome::Pass(format!("{name} (exit {want})"))
            } else {
                Outcome::Fail(format!("{name} (exit {code}, want {want})"))
            }
        }
    };
    vec![outcome]
}

/// The host either runs the link demo to completion or stays up; both are
/// healthy as long as the log is clean.
fn run_guest_link(ctx: &Ctx) -> Vec<Outcome> {
    let log_path = ctx.log.join("guest-link.log");
    let mut child = spawn_logged(ctx, "guest-link", &log_path, &["run"], &[]);
    sleep(Duration::from_secs(8));
    let outcome = match child.try_wait().expect("poll guest-link") {
        None => {
            let log = fs::read_to_string(&log_path).unwrap_or_default().to_lowercase();
            let outcome = if log.contains("error") || log.contains("panic") {
                Outcome::Fail("guest-link/run (errors in log)".into())
            } else {
                Outcome::Pass("guest-link/run (host up, clean log)".into())
            };
            let _ = child.kill();
            let _ = child.wait();
            outcome
        }
        Some(status) if status.success() => Outcome::Pass("guest-link/run (exited 0)".into()),
        Some(status) => {
            Outcome::Fail(format!("guest-link/run (exit {})", status.code().unwrap_or(-1)))
        }
    };
    vec![outcome]
}

/// Without IDENTITY_* credentials the host must refuse to start and name the
/// missing variables. Backends connect only after the guest is compiled
/// (~10s in a debug build), so allow up to 60s for the exit.
fn run_identity_fail_fast(ctx: &Ctx) -> Vec<Outcome> {
    let log_path = ctx.log.join("identity-fail-fast.log");
    let wasm = ctx.wasm.join("identity_wasm.wasm").display().to_string();
    let mut child = spawn_logged(ctx, "identity", &log_path, &["run", &wasm], &[]);
    let mut exited = None;
    for _ in 0..60 {
        if let Some(status) = child.try_wait().expect("poll identity") {
            exited = Some(status);
            break;
        }
        sleep(Duration::from_secs(1));
    }
    let outcome = match exited {
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Outcome::Fail("identity/fail-fast (still running without credentials)".into())
        }
        Some(status) if status.success() => {
            Outcome::Fail("identity/fail-fast (exit 0 without credentials)".into())
        }
        Some(status) => {
            let code = status.code().unwrap_or(-1);
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            if log.to_uppercase().contains("IDENTITY_") {
                Outcome::Pass(format!("identity/fail-fast (exit {code}, names IDENTITY_* vars)"))
            } else {
                Outcome::Fail(format!(
                    "identity/fail-fast (exit {code} without the missing-vars message)"
                ))
            }
        }
    };
    vec![outcome]
}

/// Stand-in OAuth2 token endpoint for the identity example. Listens on an
/// ephemeral loopback port for the life of the process and returns its URL.
async fn serve_token_stub() -> Result<String> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .context("binding token stub")?;
    let url = format!("http://{}/token", listener.local_addr()?);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service_fn(token_endpoint))
                    .await;
            });
        }
    });
    Ok(url)
}

/// Mint a fixed token for a client-credentials grant that authenticates with
/// the smoke credentials; anything else is an `invalid_client` error, which the
/// guest surfaces as a failed request.
async fn token_endpoint(
    request: hyper::Request<hyper::body::Incoming>,
) -> Result<hyper::Response<Full<Bytes>>> {
    let (parts, body) = request.into_parts();
    let form = String::from_utf8_lossy(&body.collect().await?.to_bytes()).into_owned();
    let authorization = parts.headers.get(AUTHORIZATION).and_then(|value| value.to_str().ok());
    let credentials_match = authorization == Some(IDENTITY_BASIC_AUTH);
    let granted = parts.method == hyper::Method::POST
        && credentials_match
        && form.split('&').any(|pair| pair == "grant_type=client_credentials");
    let (status, body) = if granted {
        (200, r#"{"access_token":"smoke-token","token_type":"bearer","expires_in":3600}"#)
    } else {
        // Never echo the header itself: a misconfigured run could point real
        // credentials at this stub, and the log ends up in CI output.
        let authorization = match authorization {
            None => "absent",
            Some(_) if credentials_match => "matched",
            Some(_) => "present, not the smoke credentials",
        };
        eprintln!(
            "token stub rejected {} {} authorization={authorization} form={form}",
            parts.method, parts.uri
        );
        (400, r#"{"error":"invalid_client"}"#)
    };
    Ok(hyper::Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from_static(body.as_bytes())))?)
}

fn stop_server(child: &mut Child, outcomes: &mut Vec<Outcome>) {
    let _ = child.kill();
    let _ = child.wait();
    for _ in 0..40 {
        if !port_open() {
            return;
        }
        sleep(Duration::from_millis(500));
    }
    outcomes.push(Outcome::Warn("port 8080 still open after stop".into()));
}

fn port_open() -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
}

/// First 200 characters, matching the old script's `head -c 200` diagnostics.
fn snippet(text: &str) -> String {
    text.chars().take(200).collect()
}
