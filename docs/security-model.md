# Security Model

Omnia exists to run untrusted or semi-trusted code — including agent-generated code — safely alongside real infrastructure. This page explains what the sandbox guarantees, how capabilities are granted, and, just as importantly, what the runtime does *not* protect against.

## The trust boundary

The boundary sits between the **guest** (WebAssembly, untrusted) and the **host** (native, trusted). Everything a guest can do, it does through a WASI interface the host explicitly linked; everything else is unreachable by construction:

- **No ambient filesystem.** A guest sees only the directories the host preopened via mounts, read-only unless marked writable.
- **No ambient network.** A guest cannot open sockets. Outbound HTTP exists only if the host linked `WasiHttp`, and then only through the host's client.
- **No process, clock, or environment escape.** Environment variables reach the guest only through `wasi:cli` argv/env the host chooses to forward, or `wasi:config`.
- **Memory isolation.** WebAssembly linear memory is bounds-checked; a guest cannot read host memory or another guest's.

Capability granting is therefore the host author's main security decision: the `hosts:` map in `runtime!` *is* the guest's permission set. A guest that only needs key-value storage should run in a host that links only `WasiKeyValue` (plus a trigger).

## Deployment inputs are trusted

The manifest sits on the *host* side of the trust boundary. Whether it arrives as an `omnia.toml` file or is assembled programmatically as an `omnia::Manifest`, it chooses which guest artifacts load, which host directories mount (including `writable`), and which interfaces the host dispatches between guests. Manifest validation is structural (at least one guest, unique ids, in-process transport) — it is **not** authorization. Never build a manifest from untrusted data.

Guest artifacts split into two trust classes:

- **Raw `.wasm`** is validated and compiled by wasmtime and runs inside the sandbox described above. It is the format to accept from less-trusted sources — the artifact can use every host capability the runtime compiled in and its imports allow, but it cannot escape the sandbox.
- **Pre-compiled `.bin`** is native code, loaded via wasmtime's `unsafe` deserialization. Wasmtime's compatibility check (rejecting artifacts built with mismatched compile-affecting settings) is *not* an authenticity check: a malicious `.bin` is arbitrary native code running with host privileges. Load `.bin` only from trusted, immutable storage — signed or digest-pinned artifacts your build pipeline produced.

The API enforces this split. The default `DeploymentBuilder` build and `GuestArtifact::wasm` are safe and accept only raw wasm; admitting a pre-compiled artifact requires an explicit `unsafe` attestation at the call site — `DeploymentBuilder`'s `unsafe build_trusted`, or `unsafe GuestArtifact::precompiled` for dynamic registration. The CLI (`omnia run`) makes that attestation itself, because everything an operator passes it is by definition an operator-privilege input. The split is by content, not by source kind: bytes embedded in the host binary (an `include_bytes!` `source:`) are classified the same way, so embedded pre-compiled bytes still require the unsafe attestation.

Two further consequences of dynamic loading:

- **Artifacts are read at startup.** A guest path can be substituted between manifest construction and load; prefer immutable or content-addressed artifact locations, especially for `.bin`.
- **Startup cost is unbounded by the runtime.** Nothing caps the manifest's guest count or artifact sizes; compilation cost at startup is bounded only by what the manifest names — another reason the manifest is an operator-privilege input.

Note also that `[link] interfaces` (top-level in the manifest, the `link:` block's `interfaces:` list in the macro, or CLI `--link`) are a deployment-level grant: the linker is shared, so a dispatched interface is wired for the *whole* deployment, and any guest importing it may call it. There is no per-guest ACL.

## Guest-requested plugin loading (`omnia:plugins/loader`)

A deployment that declares `plugin: { locations: [...] }` (or a bare `plugin: {}` beside `config:`) also links the loader capability, letting a guest request that the host load another component at run time. The design keeps every trust decision host-side:

- **Request-only, byte-free.** The interface carries names in and a typed handle out — component bytes structurally cannot cross it in either direction. A guest never supplies code; it names a package and a location, and the host's compiled-in acquirer produces the bytes. Validation, compilation, and publication all happen host-side; the requester gains no lifecycle authority (no deregister, no mutation of mounts, hosts, routes, or the seam list).
- **Acquisition is host policy.** The composition root declares one slot per location kind — installed through `Plugins::install` from the `Wiring::extend` hook, invoked once after the runtime assembles. The macro's declarative `plugin: { locations: [...] }` list (or a config file's `[[plugin.location]]` entries) carries them as deployment data, and the generated hook installs them through `Plugins::install_declared`. The built-in acquirers are `PathMounts` (named directory roots opened fail-fast at startup, read fresh on every load) and `RegistryClient` (exact package references from a declared default registry endpoint, fresh-release-preferred when a store cache is attached by hand). Every load routes structurally to its kind's slot, an empty slot refuses typed, and the reachable set is fixed by the deployment the operator chose — a guest cannot widen it. No installed slots means every load refuses.
- **Digest pins are operator-anchored.** A load may pin `sha256:<hex>`; the pin is verified against the acquired bytes *before* any wasmtime validation. Unpinned loads report the resolved digest on the returned handle (trust-on-first-use), so it can be committed as a pin. A re-load of an active package with a conflicting pin refuses.
- **Loaded plugins are always the raw-wasm trust class.** The loader compiles through the safe `GuestArtifact::wasm` path only; native (pre-compiled) bytes are sniffed and refused typed before wasmtime sees them. A deployment built through `unsafe build_trusted` never extends that attestation to loader results.
- **Call-site failure, not admission.** Admission does not require a loaded component to export a declared link interface — mixed deployments have host-only handlers beside link targets. A subsequent guest→guest call to a registered guest that exports none of the declared interfaces fails at the call site (the target is registered but unlinked). Its imports are bounded by the deployment's linked host set exactly like any late-registered guest.
- **Import-gated by worlds.** The loader links once on the shared linker, but wasmtime wires it only into guests whose world imports `omnia:plugins/loader`. A guest (including a loaded plugin) whose world does not name the import can never reach it; note the converse — a loaded plugin whose world *does* import the loader can itself request loads.

## Isolation between requests and guests

Every invocation runs in a **fresh instance in its own store**, torn down afterwards. Consequences:

- Nothing persists in guest memory between requests — no request can read another's data through the guest heap, and a compromised request state dies with the instance.
- In multi-guest deployments, guests share an engine and linker but never an instance or store. They can interact only through host-mediated dispatch, and only along interfaces named in the deployment's `[link] interfaces`, with nesting bounded by `MAX_DISPATCH_DEPTH`.
- The runtime core treats guest ids and interface names as opaque strings — no domain knowledge, no special cases a guest could exploit by name (the glossary's [Law 2](glossary.md#law-2)).

State that must persist lives behind a WASI interface (keyvalue, sql, blobstore, ...) where the host controls it.

## Resource containment

Sandboxing without resource limits is denial-of-service waiting to happen. Each invocation is bounded by:

| Limit | Variable | Default |
| ----- | -------- | ------- |
| Wall-clock time | `GUEST_TIMEOUT_MS` | 30 s (server invocations and server-rooted link hops; a command-mode chain, link hops included, is uncapped) |
| Linear memory | `MAX_MEMORY_BYTES` | 256 MiB |
| Instruction budget | `MAX_FUEL` | off (`0`) |
| Preemption granularity | `EPOCH_TICK_MS` | 10 ms |
| Dispatch nesting | `MAX_DISPATCH_DEPTH` | 8 |

Epoch interruption preempts CPU-bound guests, so an infinite loop cannot hold an executor thread past the timeout. Pool ceilings (`POOL_MAX_INSTANCES` and friends) cap aggregate resource use across concurrent requests.

## Filesystem: mounts

Mounts are the only filesystem doorway ([details](guides/multi-guest-deployments.md#mounts-giving-guests-a-workspace)):

- Explicit: a `[[mount]]` in the manifest or `--mount` on the command line. No mount, no filesystem.
- **Read-only by default**; writes require an explicit `writable`.
- Scoped: the preopen is rooted at the mounted directory. Paths cannot traverse above it.
- Shared: mounts preopen into *every* guest in a deployment — the mount set should be the union of what the deployment's guests legitimately need, kept minimal.

## Model completions: lending, not granting ambient access

The `omnia:model` design extends capability thinking to LLM backends, which are effectively untrusted executors:

- The backend gets **no ambient access**. It can touch a filesystem tree only if the guest lends one through `grants.workspace` — and that lend is a typed `wasi:filesystem` descriptor borrow from the guest's own preopen table, not a path string or integer handle a guest could forge. The host resolves it back to an authorized mount by identity.
- Host-injected tools (`read`, `list`, `write`) are **served and bounded by the host**: the names are reserved, and the backend can only execute them through the host's `ToolHost`, which requires the workspace grant and enforces read/listing bounds (genai advertises `read`/`list` when a workspace is lent; `write` stays unadvertised). Guest-declared function tools are answered by the guest itself over the completion session's streams; the backend forwards each call through the host, which enforces the declared-name allowlist, size cap, and timeout. Guests cannot impersonate the injected tools (reserved names are rejected).
- The **answer is validated by the host** against the requested format before the guest sees it — a backend cannot smuggle unvalidated output past the gate.
- Session limits bound runaway tool loops: a call budget and per-call timeout (`budget-exhausted`) and a per-result size cap (`tool-failed`); the cursor backend additionally cancels its bridge-managed run (`CancelRun`) on timeout or when a session tool call fails hard. Its tool-callback channel is loopback-only and bearer-authenticated, and callbacks route through the same host-enforced `ToolHost::call_tool` path as genai's session tools.

The net effect: a prompt-injected or misbehaving model session is confined to the lent workspace and the granted tools, exactly as a guest is confined to its linked interfaces.

## What Omnia does not protect against

Honest limits, so you can layer the right controls on top:

- **Outbound HTTP is coarse.** If `WasiHttp` is linked, the guest can request any URL the host can reach — there is no per-guest URL allow-list today. Network egress policy belongs at the infrastructure layer (network policies, egress proxies).
- **Backend credentials are host-side.** Guests never see connection strings, but any guest with the interface linked can use the backend's full capability (e.g. every bucket the Redis credential can reach). Scope service credentials to what the deployment needs.
- **Within one interface, granularity is the backend's.** `wasi:keyvalue` doesn't partition buckets per guest; guests in one deployment sharing a backend share its namespace.
- **Writable mounts are real writes.** A writable workspace lent to a model backend can be modified by the model. Review flows should mount read-only and route writes through validated tools.
- **Denial of service via legitimate traffic** is bounded per invocation, but request admission (rate limiting, auth) is upstream of the runtime.
- **Side channels** (timing, cache) are out of scope, as for most wasm runtimes.

## Defence-in-depth checklist

- [ ] Treat manifests and pre-compiled `.bin` artifacts as trusted operator inputs; never build either from untrusted data
- [ ] Accept only raw `.wasm` from less-trusted sources, and run it with minimal hosts and read-only mounts
- [ ] Pin plugin loads by digest wherever the artifact is known ahead of time; treat an unpinned load's reported digest as the pin to commit
- [ ] Give the loader import only to worlds that genuinely request loads; keep loadable-plugin worlds free of it
- [ ] Link only the interfaces each deployment's guests need
- [ ] Mount the minimum directory set, read-only unless writes are required
- [ ] Keep resource ceilings meaningful for the workload (don't blanket-raise timeouts and memory)
- [ ] Scope backend service credentials narrowly; prefer per-deployment credentials
- [ ] For model workloads, prefer read-only workspaces; treat `writable` lends as privileged
- [ ] Run the host container as non-root with a minimal image (see [Deploying Omnia](guides/deployment.md#container-images))
