# Getting started

Riff is a library embedded into applications that supply the microphone, playback, interface, credentials, and context. The [web demo](../examples/riff-web/README.md) is a runnable embedding; the snippets below show session wiring for each binding, with application-specific objects such as `microphone`, `speaker`, `ui`, and `myHost` left to the embedder.

Run the shell commands from the repository root. The TypeScript bundle import below is also relative to that root.

## TypeScript

Use Node.js 22.18 or newer.

```bash
npm install
npm run build
npm test
```

```ts
import { loadBundle, RiffSession } from "@riff/core";
import { OpenAIRealtimeProvider, clientSecretCredentials } from "@riff/openai-realtime";
import { GitHubHost, issueDestination } from "@riff/github";
import bundleJson from "./core/dist/riff-agent.bundle.json" with { type: "json" };

const session = new RiffSession({
  bundle: loadBundle(bundleJson),
  provider: new OpenAIRealtimeProvider({
    // Your backend mints this. A long-lived API key never reaches a client.
    credentials: clientSecretCredentials(() => fetch("/api/riff/token").then((r) => r.json())),
  }),
  host: new GitHubHost({
    token: process.env.GITHUB_TOKEN!,
    repository: "acme/web",
    destinations: [issueDestination({ repository: "acme/web", default: true })],
  }),
});

session.on((event) => {
  if (event.type === "agent.audio") speaker.play(event.audio);
  if (event.type === "draft") ui.showDraft(event.gist, event.fidelity);
  if (event.type === "submitted") ui.confirm(event.artifact);
});

await session.start();
microphone.on("chunk", (pcm) => session.sendAudio(pcm));
```

> [!IMPORTANT]
> Keep the credential-bearing `GitHubHost` on your backend, never in a browser or device bundle. For a browser session, use a host proxy like the demo's [`proxy-host.js`](../examples/riff-web/public/proxy-host.js). Devices receive ephemeral client secrets from your backend; [`mintClientSecret`](../packages/riff-openai-realtime/src/client-secret.ts) stays server-side. See [providers.md](providers.md#credentials) and [security.md](security.md#credentials).

## Swift

The Swift package needs macOS to build and run its tests because `RiffAudio` uses `AVFoundation`.

```bash
swift test --package-path swift
```

```swift
import RiffCore
import RiffOpenAIRealtime
import RiffAudio

let session = RiffSession(
    bundle: try AgentBundle.bundled(),
    provider: OpenAIRealtimeProvider(
        credentials: .clientSecret { try await backend.mintRiffToken() }
    ),
    host: myHost
)

let audio = RiffAudioEngine()
audio.onCapture = { pcm in Task { @MainActor in session.send(audio: pcm) } }

try await session.start()
try audio.start()

for await event in session.events {
    switch event {
    case .agentAudio(let pcm): audio.play(pcm)
    case .interrupted: audio.flushPlayback()
    case .draft(_, let ready, let fidelity, let gist): ui.show(gist, ready, fidelity)
    case .submitted(let artifact): ui.confirm(artifact)
    default: break
    }
}
```

Hold a strong reference to the session for as long as it is connected. Releasing it tears the connection down by design; a half-live session with an open microphone is worse than a closed one.

## Rust

```bash
cargo test --manifest-path rust/Cargo.toml
```

```rust
let bundle = Arc::new(AgentBundle::bundled()?);
let mut options = RiffSessionOptions::new(bundle, provider);
options.host = Arc::new(my_host);

let mut session = RiffSession::new(options);
session.on(Box::new(|event| match event {
    RiffEvent::AgentAudio(pcm) => speaker.play(pcm),
    RiffEvent::Draft { gist, ready, fidelity, .. } => ui.show(gist, *ready, *fidelity),
    RiffEvent::Submitted(artifact) => ui.confirm(artifact),
    _ => {}
}));

session.start().await?;
session.run().await;
```

Neither crate has a third-party dependency, and neither brings an async runtime. The socket, the microphone, and the executor are all yours. See the [Rust README](../rust/README.md) for the runtime seams and pinned contributor toolchain.

## Integration and validation

The [host bridge](host-bridge.md) supplies references, domain vocabulary, earlier prompts, and submission destinations. The [provider interface](providers.md) supplies speech without exposing vendor wire formats to the engine. The [architecture](architecture.md) covers their boundaries and the session lifecycle.

The three bindings load the same agent definition and execute the same conformance cases. See [conformance.md](conformance.md#running) for the shared test commands and [AGENTS.md](../AGENTS.md#build-test-lint) for contributor validation requirements.
