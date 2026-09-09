# Model Interface Reference

Reference for the `omnia:model/completion` interface (version `0.1.0`): the request/reply types guests use, the request checks the host enforces, and the `check` callback through which the guest judges answers. The conceptual walk-through is in [Model Completions and MCP](../guides/model-completions.md); the authoritative WIT is [`crates/wasi-model/wit/model.wit`](../../crates/wasi-model/wit/model.wit).

## The function

```wit
create: async func(request: request, results: stream<tool-result>) -> result<session, error>;
```

One request opens one **session**. The guest creates the `results` stream, keeps the writer, and passes the readable end; the host returns the session:

```wit
record session {
    calls: stream<tool-call>,
    reply: future<result<reply, error>>,
}
```

Tool calls stream host-to-guest on `calls`; the guest answers each on `results` by `id` (calls may arrive in parallel and results are unordered). When the request sets `check`, each candidate answer also arrives on `calls` as a tool call named `check` (see below). The `reply` future resolves when the backend finishes — the host always resolves it, so budget and deadline failures arrive as typed `error` values, never as a dropped writer. When the completion finishes (or its call budget is exhausted) the host closes `calls`, ending the guest's tool loop.

Most guests never touch this machinery: the `omnia-guest` `Model::complete` / `Model::complete_with` sugar runs the session dance and hands tool calls to a closure — see the [guide](../guides/model-completions.md).

### Session limits

The host enforces per-session limits (`Limits`, backend-configurable via `WasiModelCtx::limits`): a tool-call budget (default 32, `budget-exhausted`), a per-result size cap (default 1 MiB, `tool-failed`), and a per-call timeout (default 60 s, `budget-exhausted`). There is no streaming variant of `create` yet.

## Request

| Field | Type | Meaning |
| ----- | ---- | ------- |
| `model` | `option<string>` | Opaque model id hint, passed through unchanged; the backend may override. `None` defers entirely to the backend (genai defaults to its configured model). |
| `system` | `option<string>` | System/instructions channel. |
| `messages` | `list<message>` | Chat turns. **Must not be empty.** |
| `generation` | `option<generation>` | Sampling and length controls; omitted fields defer to backend defaults. |
| `format` | `format` | Output shape hint steering the provider (see below). |
| `tools` | `list<tool>` | Guest-declared functions and MCP grants. |
| `grants` | `grants` | Capabilities lent for this call (see below). |
| `check` | `bool` | Ask the guest to accept each candidate answer before it becomes the reply (see below). |

### `message`

`role` (`system` \| `user` \| `assistant`) plus `content` (turn text). The guest-side `Sections` builder assembles `system`/`messages` from structured fields (role, task, context, ...) so prompts stay consistent — see the [guide](../guides/model-completions.md#requesting-a-completion-from-a-guest).

### `format`

| Variant | Steering |
| ------- | -------- |
| `text` | Plain text. |
| `json` | Toward a JSON object. |
| `schema(schema)` | Toward the given JSON Schema. `schema` carries a `name` (passed to the provider, e.g. `verdict`) and the schema document as a JSON string, which must parse as JSON (`invalid-request` otherwise). |

`format` is a hint: backends pass it to the provider as `response_format` where the provider constrains decoding, or as instruction prose where it does not, and extract the candidate from the model's final text (the whole text when it parses, else the last fenced or brace-delimited JSON value). Nothing validates the answer against it. Acceptance is the guest's, through `check`.

### `check`

When `check` is set, the backend does not finish on the model's final text. It offers each candidate to the guest over the session: a `tool-call` named **`check`** arrives on `calls` with the candidate text as its `arguments`, and the guest answers on `results`:

| Result | Effect |
| ------ | ------ |
| `ok(_)` | The candidate is the reply; the completion ends. |
| `err(text)` | The backend appends `text` verbatim as the next user turn (after the rejected candidate as an assistant turn) and goes round again. |

The check rides the same streams as function tools but outside their budget: it never counts against `max-tool-calls` and needs no declaration in `tools`. The per-call timeout does apply, and the per-result size cap bounds the correction text (`tool-failed` when exceeded). A backend that runs out of rounds on a rejection fails the completion with `budget-exhausted` whose detail is the last correction. Guests using `omnia-guest` reach this through `Request::check` and a `complete_with` handler matching `call.name == "check"`, or through `model::Question<T>`, which derives the steering schema from `T`, deserializes each candidate, runs the guest's closure, and returns the accepted `T`.

### `generation`

`temperature`, `top-p`, `max-tokens`, `stop` (halt sequences), `seed`, and `effort` — a reasoning-effort hint (`minimal` \| `low` \| `medium` \| `high`) for thinking-capable models. All optional except `stop` (which may be empty).

### `tool`

| Variant | Fields | Support |
| ------- | ------ | ------- |
| `function` | `name`, `description`, `parameters` (JSON Schema for the arguments object) | Advertised to the provider and executed by the guest through the session's `calls`/`results` streams (genai and cursor) |
| `mcp` | `name`, `tools` (allowlist; empty = all), `url` (server endpoint) | Cursor backend only; genai rejects MCP grants. The allowlist is advisory (prompt-level), not enforced |

Function names must not collide with the reserved names — the host-injected tools below and `check` — and `parameters` must parse as JSON (`invalid-request` otherwise).

### `grants`

| Field | Type | Effect |
| ----- | ---- | ------ |
| `workspace` | `option<workspace-grant>` | A `wasi:filesystem` directory descriptor from the guest's own preopen table plus a relative `subpath`. Being a typed resource borrow, it cannot be forged — the host resolves it back to an authorized mount by directory identity, then exposes it to backends as bounded `read`/`list`/`write` (genai) or the absolute local path (the cursor agent's working directory). |

### Host-injected tools

The names **`read`**, **`list`**, **`write`** (the host's workspace tools) and **`check`** (the guest's answer check) are reserved; guests must not declare tools with them (`invalid-request`). When the guest lends `grants.workspace`, the genai backend advertises `read` and `list` to the model and executes them host-side through the `ToolHost` workspace capability — bounded reads and listings that never traverse the session. Read results must be UTF-8 text and, like session tool results, fit the per-result byte cap. `write` stays reserved and served by `ToolHost` but is not yet advertised to the model. The cursor backend advertises none of these: its bridge-managed agent inspects the workspace natively through the local path. Declared function tools travel a different road: the backend forwards the model's call through `ToolHost::call_tool`, the host checks the name against the request's declared tools and enforces the session limits, and the guest's handler answers over the session streams.

## Reply

| Field | Type | Meaning |
| ----- | ---- | ------- |
| `answer` | `string` | The text the guest's `check` accepted — or, without a check, the model's final text as the backend received it. |
| `usage` | `option<usage>` | Token accounting when the backend reports it: `input-tokens`, `output-tokens`, optional `reasoning-tokens`. |

## Errors

| Variant | Meaning | Retry? |
| ------- | ------- | ------ |
| `invalid-request(string)` | The request is malformed (empty `messages`, reserved tool name, schema that is not JSON). | Not without changing the request. |
| `budget-exhausted(string)` | Iteration, token, or time budget ran out. When the last round ended on a rejected `check`, the detail is that correction text. | With a larger budget, or a prompt the model can satisfy. |
| `tool-failed(string)` | A tool call failed non-repairably. | Depends on the tool. |
| `backend(string)` | Transport, process, or provider failure. | Usually transient. |

## Backends implementing this interface

| Backend | Location | Notes |
| ------- | -------- | ----- |
| `ModelDefault` | in-tree (`wasi-model`) | Deterministic echo: text/json answer with the prompt; `format::schema` errors; honours `check` once (a rejection is `budget-exhausted`) |
| `omnia-genai` | omnia-backends repo | Provider APIs in-process; drives the function-tool session loop and the `check` loop; no MCP |
| `omnia-cursor` | omnia-backends repo | Bridge-managed Cursor agent (`cursor-sdk-bridge`); function tools bridge into the session as SDK custom tools; MCP grants pass inline; workspace optional (tools-only in a private directory when unlent); 120s default inactivity timeout |
