# Riff for Rust

The Rust binding: [`riff-core`](riff-core/) is the engine, [`riff-openai-realtime`](riff-openai-realtime/) is a provider.

It is a peer of the [TypeScript](../packages/) and [Swift](../swift/) bindings, not a port of either. All three read the same compiled agent definition from [`core/`](../core/) and reproduce the same [conformance cases](../core/conformance/cases/), which is what makes "the same agent everywhere" checkable rather than aspirational.

```sh
cargo test --manifest-path rust/Cargo.toml
```

## What you supply

Riff is a library, not an application, and the standard library has no async runtime, socket, or HTTP client. So four things are seams rather than dependencies:

| Trait | What it is for |
|---|---|
| [`RealtimeProvider`](riff-core/src/provider.rs) | Produces speech. `riff-openai-realtime` is one. |
| [`RiffHost`](riff-core/src/host.rs) | Knows the speaker's world: what "the PR I just opened" is, and where a finished prompt goes. |
| [`RiffStore`](riff-core/src/host.rs) | Persists what outlives a session. [`MemoryStore`](riff-core/src/host.rs) is the default. |
| [`Clock`](riff-core/src/runtime.rs) | Time and delays. [`SystemClock`](riff-core/src/runtime.rs) works anywhere; implement it over your runtime's timer if you have one. |

Async methods are ordinary futures the embedder drives. Nothing here spawns a task, so the engine runs on tokio, on `async-std`, or on a hand-rolled executor without knowing the difference. [`RiffSession::run`](riff-core/src/session.rs) is the event pump; [`RiffSession::step`](riff-core/src/session.rs) handles one event, for an embedder that wants to own the loop.

```rust,no_run
use std::sync::Arc;
use riff_core::{AgentBundle, RiffSession, RiffSessionOptions};

let bundle = Arc::new(AgentBundle::bundled()?);
let mut options = RiffSessionOptions::new(bundle, provider);
options.host = Arc::new(my_host);

let mut session = RiffSession::new(options);
session.on(Box::new(|event| render(event)));
session.start().await?;
session.run().await;
```

## Conventions

- **No third-party dependencies, in either crate.** `riff-core` has none and `riff-openai-realtime` depends only on `riff-core`, so [`Cargo.lock`](Cargo.lock) is three packages long. JSON, text normalization, base64, RFC 3339 formatting, and the deadline primitive are written against the standard library. A dependency here is a dependency the embedder did not choose; adding one needs a stated reason in the PR.
- **`unsafe` is forbidden**, by a workspace lint rather than by convention.
- **Everything public is documented**, by `missing_docs`.
- **`cargo fmt` and `cargo clippy -D warnings` gate CI.** Unlike the TypeScript and Swift halves of this repo, Rust has one canonical formatter and linter, so the binding is held to them.

## Where the bindings deliberately differ

Behavior a speaker can observe is identical, and the conformance suite proves it. These are the places the *shape* differs, because the language does:

- **Cancellation is by drop.** The other bindings hand a host an `AbortSignal` or a cooperative `Task` cancellation. In Rust, [`with_deadline`](riff-core/src/runtime.rs) drops the in-flight future, which cancels it at its next suspension point. A host whose work has a side effect the speaker can see must still make it idempotent — the send may already be in flight when the model is told it failed.
- **Tools report effects rather than calling back.** [`ToolRuntime::dispatch`](riff-core/src/tools.rs) returns a [`ToolEffect`](riff-core/src/tools.rs) list that the session turns into events, instead of invoking closures the way the TypeScript runtime does. Same sequence, no shared mutable state between the two halves.
- **Hash order is never load bearing.** Where the other bindings lean on an insertion-ordered map, this one sorts explicitly — motif ids, the biasing comparator, and [`JsonObject`](riff-core/src/json.rs), which keeps insertion order so a serialized tool result is stable between runs.
- **Unknown JSON Schema keywords are refused at load.** [`bundle.rs`](riff-core/src/bundle.rs) rejects a tool whose parameters use a keyword [`schema.rs`](riff-core/src/schema.rs) does not enforce. A constraint that is declared in `core/agent` and silently unenforced here would be worse than one that is absent, because every other binding would apply it.
