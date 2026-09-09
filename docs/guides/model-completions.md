# Model Completions and MCP

The `omnia:model/completion` interface lets a sandboxed guest request a completion from a large language model without knowing which model, provider, or agent serves it. The guest states *what* it wants (prompt, response format, capability grants); the host checks the request, injects tools, and runs the backend. Whether an answer is *acceptable* is the guest's call: a request may ask to `check` each candidate, and the backend keeps going until the guest says yes.

This page covers the guest API, the grants model, the available backends, and how guests can also *serve* tools to models over MCP (the [Model Context Protocol](https://modelcontextprotocol.io)).

> **Host prerequisite.** The runtime serving your guest must link this interface: add `WasiModel: ModelDefault` (from `omnia_wasi_model`) to the `runtime!` `hosts:` map — see [Composing a Runtime](composing-a-runtime.md). Note the default backend only echoes prompts; bind `omnia-genai`/`omnia-cursor` for real completions, or define an inline canned backend in tests (see [Backends](#backends)).

## Requesting a completion from a guest

A model guest is typically a command-mode guest (see [Writing Guests](writing-guests.md#command-mode-guests)). It builds a `Request` and calls `Model::complete` on the `WasiModel` handle:

```rust,noplayground
use omnia_guest::model::{Format, Message, Model as _, Request, Role, SchemaFormat, WasiModel};

let (system, user) = Sections {
    role: Some("a terse code reviewer".to_string()),
    task: "decide whether the change is acceptable".to_string(),
    context: Some("the diff adds a bounds check".to_string()),
    ..Sections::default()
}
.assemble(None);

let request = Request::builder()
    .maybe_system(system)
    .messages(vec![Message { role: Role::User, content: user }])
    .format(Format::Schema(
        SchemaFormat::builder().name("verdict").schema("{\"type\":\"object\"}").build(),
    ))
    .build();

let answer = match WasiModel.complete(request).await {
    Ok(reply) => reply.answer,
    Err(error) => format!("error: {error:?}"),
};
```

The pieces:

- **`Sections`** — a guest-side builder that assembles the `system` and `messages` channels from structured fields (role, task, context, ...), so prompts stay consistent across guests.
- **`model: None`** — which concrete model serves the request is deployment configuration; the guest can suggest a model id but usually leaves it to the backend.
- **`format`** — `Text`, `Json`, or `Schema` with a JSON Schema. A steering hint for the provider (`response_format` where it constrains decoding, instruction prose where it does not); nothing validates the answer against it. To judge answers, ask for a check (below).
- **`tools`** — functions the guest itself declares for the model to call, or MCP servers to attach (backend-dependent; see below).
- **`grants`** — capabilities the guest lends to the completion.

### Answering tool calls

A request that declares function tools uses `Model::complete_with`, which runs the completion **session**: the model's tool calls stream back to the guest, a closure answers each one over guest state, and the future resolves with the reply once the model finishes:

```rust,noplayground
let outcome = WasiModel
    .complete_with(request, |call: ToolCall| async move {
        SHELF
            .iter()
            .find(|(key, _)| *key == call.arguments)
            .map(|(_, value)| (*value).to_owned())
            .ok_or_else(|| format!("no shelf value for `{}`", call.arguments))
    })
    .await;
```

The closure's `Err` is model-visible failure text the model may repair from; hard failures (budget, timeouts, oversized results) arrive as typed errors on the reply. Under the hood this is the `omnia:model/completion` session — `create` returns a `calls` stream and a `reply` future, and the guest answers on a `results` stream (see the [reference](../reference/model.md)); the sugar runs that dance so most guests never touch it. The host bounds every session: a tool-call budget, a per-result size cap, and a per-call timeout, all backend-configurable.

### Judging the answer: `check` and `Question<T>`

The type a guest wants back cannot cross the WIT boundary, but a callback can. A request built with `.check(true)` asks the backend to offer every candidate answer to the guest before finishing: the candidate arrives at the `complete_with` handler as a `ToolCall` named `check` whose `arguments` are the candidate text. Returning `Ok` accepts it as the reply; returning `Err(text)` sends `text` back verbatim as the correction turn and the backend goes round again (bounded by its own round budget — a final rejection surfaces as `Error::BudgetExhausted` carrying the guest's last correction). The check needs no declaration in `tools` and does not count against the tool-call budget.

`omnia_guest::model::Question<T>` runs that exchange for a typed answer. `T` derives `Deserialize` and `JsonSchema` (the guest depends on `schemars` 1.x, the version `omnia-guest` builds against); the question steers the provider with `T`'s schema, deserializes each candidate, hands it to the guest's closure, and returns the accepted `T`:

```rust,noplayground
use omnia_guest::model::{Question, WasiModel};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct Verdict {
    verdict: String,
    findings: Vec<String>,
}

let verdict = Question::<Verdict>::new("verdict")
    .system("a terse code reviewer")
    .ask(&WasiModel, "decide whether the change is acceptable", None, |verdict| {
        if verdict.verdict == "pass" || !verdict.findings.is_empty() {
            Ok(())
        } else {
            Err(vec!["a `fail` verdict must list its findings".to_owned()])
        }
    })
    .await?;
```

The closure's `Err(findings)` becomes the correction turn (one `## Previous answer (rejected)` / `## Findings` template by default; `Question::correction(|previous, findings| ..)` renders it differently for one question); a candidate that does not deserialize as `T` is corrected the same way, and a completion that ends without an accepted answer after such a mismatch is `Error::InvalidRequest` — the schema and the type disagree, which more rounds would not fix. `Question::tools(..)` declares function tools and `ask`'s `tools` argument handles them; `Question::schema(|schema| ..)` post-processes the steering schema (enums, counts, patterns) without touching `T`.

## Grants and host-injected tools

Grants are the security boundary. Rather than giving the model backend ambient access, the guest explicitly lends:

- **`workspace`** — a directory descriptor from the guest's own preopen table (populated by the host's `[[mount]]`; see [Multi-Guest Deployments](multi-guest-deployments.md#mounts-giving-guests-a-workspace)). The model can only see a tree the host mounted *and* the guest chose to lend.

This grant is what drives the host-injected tools. The names `read`, `list`, and `write` are reserved — guests must not redeclare them in `tools`, nor `check`, the answer callback's name — and the host serves them through the per-completion `ToolHost`, bounded to the lent tree. When a workspace is lent, the genai backend advertises `read` and `list` to the model and executes them host-side, never through the session; `write` stays reserved but is not yet advertised. The cursor backend skips them entirely — its agent inspects the workspace natively. Guest-declared function tools also execute through the `ToolHost` (`call_tool`), so every invocation passes through the host's declared-tool check and session limits.

## Backends

### `ModelDefault` — deterministic echo (in-tree)

The default backend connects with zero configuration and answers every completion with its own prompt: the last message echoed as a string for `format::text`, wrapped as `{"echo": ...}` for `format::json`. That makes guest wiring and prompt assembly smoke-testable with no live model. `format::schema` requests fail with a `backend` error — no echo can conform to an arbitrary guest schema — so bind a real backend for typed answers. A `check` request gets its one candidate offered once; a rejection is `budget-exhausted`.

For tests, CI, and local development of model guests, define an inline `WasiModelCtx` impl that returns a fixed answer — no network, no credentials, fully deterministic, and (unlike an echo) able to satisfy `format::schema` (see the recipe in [Testing Policy](testing-policy.md#canned-model-backends)). The [`model` example](../../examples/model/) serves its fixed schema answer that way:

```bash
cargo build --example model-wasm --target wasm32-wasip2
cargo run --example model
```

### `omnia-genai` — provider APIs (omnia-backends repo)

Calls LLM provider APIs in-process via the [`genai`](https://crates.io/crates/genai) SDK (OpenAI, Anthropic, Gemini, Groq, Ollama, and others). Provider API keys are read from the environment at call time (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, ...). It advertises the request's declared function tools — plus the host-injected `read`/`list` workspace tools when the guest lent a workspace — and drives the bounded session tool loop: `read`/`list` execute host-side against the lent tree, while every other model tool call is forwarded back through the host to the guest's handler. When the request asks for a `check`, each final text is offered to the guest and a rejection is appended to the provider conversation as the next user turn. MCP tools are not supported by this backend — use `omnia-cursor` for that.

### `omnia-cursor` — Cursor SDK bridge (omnia-backends repo)

Drives one bridge-managed Cursor agent per completion through [`cursor-sdk-bridge`](https://github.com/cursor/sdk-bridge), giving the model a full agentic session inside the granted workspace:

- Requires `cursor-sdk-bridge` on `PATH` and `CURSOR_API_KEY`.
- The agent runs in the directory behind the guest's `grants.workspace` mount; without one, the completion still runs tools-only in a private empty directory with every built-in tool disabled.
- `Tool::Function` declarations become SDK custom tools: the agent's calls route back through the backend's loopback callback into the session (`ToolHost::call_tool`), so the guest's handler answers under the host's session limits.
- `Tool::Mcp` grants pass inline as the agent's `mcp_servers`; nothing is written into the workspace.
- A `check` request offers each agent answer to the guest and sends a rejection back into the same agent session as the next prompt.

Wire it like any other backend:

```rust
use omnia_cursor::Client as Cursor;

omnia::runtime!({
    mode: command,
    hosts: {
        WasiHttp: HttpDefault,
        WasiOtel: OtelDefault,
        WasiModel: Cursor,
    }
});
```

The end-to-end demo lives at [`omnia-backends/examples/cursor`](https://github.com/augentic/omnia-backends/tree/main/examples/cursor).

## Serving MCP tools from a guest

Guests can also sit on the other side of the protocol: exposing tools and resources to model backends as a stateless MCP server over HTTP. Implement `omnia_guest::mcp::McpServer` and serve `mcp::router` from the guest's HTTP handler:

```rust,noplayground
struct HttpGuest;
wasip3::http::service::export!(HttpGuest);

impl Guest for HttpGuest {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        omnia_wasi_http::serve(mcp::router(Docs), request).await
    }
}
```

The `McpServer` trait has five methods: `info` (server identity), `tools` (tool declarations with JSON Schema inputs), `call_tool`, and optionally `resources`/`read_resource`. The router handles the JSON-RPC and Streamable HTTP transport details.

The [`mcp`](../../examples/mcp/) example serves a small document set; combined with the cursor backend and an MCP grant, a completion can call back into guest-served tools — a guest-to-model-to-guest loop entirely under host mediation.
