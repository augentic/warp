# Messaging

The `wasi:messaging` interface gives guests topic-based publish/subscribe, request-reply, and an inbound message trigger. The in-memory default (`MessagingDefault`, a broadcast channel) makes all three patterns runnable with zero infrastructure; Kafka (`omnia-kafka`) and NATS (`omnia-nats`) are drop-in production backends.

The [`messaging`](../../examples/messaging/) example demonstrates every pattern below with the default backend.

> **Host prerequisite.** The runtime serving your guest must link this interface: add `WasiMessaging: MessagingDefault` (from `omnia_wasi_messaging`) to the `runtime!` `hosts:` map — see [Composing a Runtime](composing-a-runtime.md). Everything below is guest code.

## Publishing

Connect a client by name, build a `Message`, and send it to a topic:

```rust
use omnia_wasi_messaging::types::{Client, Message};
use omnia_wasi_messaging::producer;

let client = Client::connect("default".to_string()).await?;
let message = Message::new(&payload_bytes);
message.set_content_type("application/json");
message.add_metadata("key", "example_key");

producer::send(&client, "orders.created".to_string(), message).await?;
```

Metadata and content type travel with the message; on Kafka, `add_metadata("key", ...)` also drives partition assignment.

## Handling incoming messages

Messaging is a **trigger**: the host (`WasiMessaging`) subscribes to topics and delivers each message to the guest's exported handler, instantiating a fresh guest instance per message:

```rust,noplayground
use omnia_guest::api::messaging::{Router, consume};
use omnia_guest::api::{Client, Context};
use omnia_wasi_messaging::types::{Error, Message};

pub struct Messaging;
omnia_wasi_messaging::export!(Messaging with_types_in omnia_wasi_messaging);

async fn create_order(
    input: CreateOrder, context: Context<MyProvider>,
) -> Result<(), omnia_guest::Error> {
    // ...
}

fn router() -> Router<MyProvider> {
    Router::new(Client::new("acme", MyProvider))
        .route("orders.created", consume(create_order))
}

impl omnia_wasi_messaging::incoming_handler::Guest for Messaging {
    async fn handle(message: Message) -> anyhow::Result<(), Error> {
        omnia_guest::api::messaging::handle(&router(), message).await
    }
}
```

The guest router matches registered topics exactly; broker subscription patterns remain host configuration (`KAFKA_TOPICS`, `NATS_TOPICS`). `consume` decodes JSON and acknowledges successful handler output. The current WIT handler returns only `result<_, error>`: `Ok(())` acknowledges, while handler failures return `error.other` for host-defined retry or rejection behavior. In multi-guest deployments, each guest's `routes.messaging` patterns select it by NATS-style topic match — see [Multi-Guest Deployments](multi-guest-deployments.md#routing-inbound-traffic).

For non-JSON payloads, register the route with `consume_with(handler, decode)` instead: the decoder `Fn(&Delivery) -> Result<I, DecodeError>` sees the whole delivery (payload, `content_type`, metadata), and a decode failure rejects the message just as malformed JSON does under `consume`.

## Request-reply

The requester sends and awaits a reply on the same call; the handler replies to the inbound message:

```rust
// Requester
let reply = request_reply::request(&client, "quotes".to_string(), &message, None).await?;
let first = reply.first().context("empty reply")?;

// Handler (inside incoming_handler::Guest::handle)
let reply = Message::new(response_bytes);
request_reply::reply(&message, reply).await?;
```

The final `request` argument is an optional timeout; `None` defers to the backend default.

## Fan-out and async work

Handlers can publish further messages, including many at once. Use `wit_bindgen::spawn_local` for concurrent sends and `yield_async` to keep the instance cooperative:

```rust
for i in 0..1000 {
    wit_bindgen::spawn_local(async move {
        let client = Client::connect("default".to_string()).await?;
        producer::send(&client, "downstream".to_string(), make_message(i)).await
    });
}
```

A handler can also make outbound HTTP calls mid-message (the example's topic `c` handler does exactly that) — capabilities compose freely inside one guest.

## Combining with HTTP

A single guest can export both the HTTP handler and the messaging handler — a common shape where REST endpoints enqueue work and the messaging handler processes it. The example's HTTP routes (`/pub-sub`, `/request-reply`) each drive one messaging pattern.

## Backends

| Backend | Notes |
| ------- | ----- |
| `MessagingDefault` (in-tree) | In-process broadcast; delivery only within the runtime process |
| `omnia-kafka` | Apache Kafka; `KAFKA_BROKERS`, `COMPONENT`, `KAFKA_TOPICS`, `KAFKA_CONSUMER_GROUP`, SASL via `KAFKA_USERNAME`/`KAFKA_PASSWORD`, optional Schema Registry (`KAFKA_REGISTRY_URL`) |
| `omnia-nats` | NATS core; `NATS_ADDR`, `NATS_TOPICS`, NKey auth via `NATS_JWT`/`NATS_SEED` |

Semantics to keep in mind when moving from the default to a broker: the in-memory backend is at-most-once within one process, while Kafka/NATS bring their own delivery, ordering, and consumer-group semantics. Guest code is unchanged, but handlers should be idempotent if the production broker can redeliver.

## WebSockets

The `wasi:websocket` interface is the same trigger shape pointed at WebSocket connections: the host (`WasiWebSocket`, default backend `WebSocketDefault`, a tungstenite server on `WEBSOCKET_ADDR`) accepts connections and delivers inbound frames to the guest's exported handler, and guests push events out through a client:

```rust
use omnia_wasi_websocket::types::{Client, Event};
use omnia_wasi_websocket::client;

// Push to connected clients (None = broadcast; Some(group) targets a group)
let ws = Client::connect("default".to_string()).await?;
client::send(&ws, Event::new(&payload), None).await?;

// Receive inbound frames
struct WebSocket;
omnia_wasi_websocket::export!(WebSocket);

impl omnia_wasi_websocket::handler::Guest for WebSocket {
    async fn handle(event: Event) -> Result<(), Error> {
        // react, or echo back with client::send
        Ok(())
    }
}
```

The [`websocket`](../../examples/websocket/) example pairs an HTTP control endpoint (POST a message) with a WebSocket broadcast to all connected clients. In manifests, `routes.websocket` entries use the same pattern syntax as messaging routes.
