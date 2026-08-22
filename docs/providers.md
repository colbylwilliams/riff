# Providers

A provider is everything that turns speech into speech. It is the lowest layer of Riff and the one
most likely to be replaced, so the interface is kept as small as the job allows.

## The contract

```ts
interface RealtimeProvider {
  readonly id: string;
  readonly capabilities: ProviderCapabilities;
  connect(request: ConnectRequest): Promise<RealtimeConnection>;
}

interface RealtimeConnection {
  sendAudio(chunk): void;
  commitAudio(): void;
  sendText(text, options?): void;
  respondToTool(callId, resultJson): void;
  requestResponse(): void;
  cancelResponse(): void;
  updateSession(patch): void;
  on(listener): () => void;
  close(reason?): Promise<void>;
}
```

Events flow back as a neutral `ProviderEvent` union: connection, speech start and stop, input
transcripts, response audio and text, tool calls, rate limits, errors, close.

That is the whole surface. Nothing above it knows any wire format, which is what makes the swap
real rather than nominal: a different engine changes nothing a speaker can observe, because the
ledger, the grounding check, drafts, takes, and the artifact all sit above this line.

## Requirements

**Input transcription is mandatory.** The prompt body is assembled from what the speaker said, so a
provider that produces speech without giving back a verbatim transcript cannot support Riff. This is
the one capability that is not optional.

**Tool calls are delivered per turn.** The `tool.calls` event carries every call of one model turn.
Riff runs them all, returns every result, then asks for exactly one continuation. Emitting them one
at a time would produce one spoken reply per tool, which sounds like the agent stuttering.

**Barge-in should be supported.** Without it the agent talks over someone who has started speaking.
Riff still calls `cancelResponse()`, but a provider that ignores it will feel wrong.

**Semantic turn detection is strongly preferred.** Silence-based endpointing treats a thinking pause
as the end of a turn, which is the specific failure that makes voice agents unpleasant. Providers
without it get `serverVadFallback` with a deliberately long silence window.

## The OpenAI Realtime provider

Targets the GA interface. Several differences from the beta interface fail quietly rather than
loudly, so they are worth stating:

| | Beta | GA |
|---|---|---|
| Session discriminator | absent | `type: "realtime"` required |
| Audio format | `"pcm16"` | `{ "type": "audio/pcm", "rate": 24000 }` |
| Modalities field | `modalities` | `output_modalities` |
| Audio delta event | `response.audio.delta` | `response.output_audio.delta` |
| Transcript delta | `response.audio_transcript.delta` | `response.output_audio_transcript.delta` |
| Beta header | `OpenAI-Beta: realtime=v1` | removed |

A client listening for the beta event names connects successfully and then sits in silence, so the
event mapper handles both and is written against the GA names.

Other details the mapping gets right:

- **Function calls are read from `response.done`**, not from the streaming argument deltas, because
  that is the only event guaranteed to carry every call of a turn at once.
- **Tool results are `conversation.item.create` with `type: "function_call_output"`**, keyed by
  `call_id` — not the item id — with `output` as a JSON string.
- **`reasoning` and `parallel_tool_calls` are gated to reasoning models.** Non-reasoning models
  reject them outright.
- **Vocabulary biasing depends on the transcription model.** `gpt-4o-transcribe` takes a free-text
  `prompt`, `gpt-live-transcribe` takes a `keywords` array, and some models take neither. The
  provider keeps a capability table and emits the right field.
- **`truncation.token_limits.post_instructions`** keeps the instructions from being truncated away
  in a long session. Losing them mid-conversation would quietly disable every rule Riff runs on.

### Transports

**WebSocket** — base64 PCM16 at 24 kHz inside JSON events. Works everywhere with no media stack,
which makes it the transport for servers, tests, terminals, and any platform without WebRTC. This is
what the Swift package uses, over `URLSessionWebSocketTask`.

**WebRTC** — events on an `oai-events` data channel, audio on a media track. Better on a phone or in
a browser because the media stack handles jitter and packet loss that raw PCM over a socket does
not. Note that interrupting also needs `output_audio_buffer.clear`, or buffered speech keeps playing
over the person who interrupted; the provider does this automatically.

### Credentials

Clients connect with ephemeral secrets. `mintClientSecret` is the only part of the provider that
must run server-side, and it is the reason no long-lived key ever reaches a device.

```ts
// Your backend, behind your own auth
const secret = await mintClientSecret({
  apiKey: process.env.OPENAI_API_KEY!,
  session: bundle.session,
  instructions: bundle.instructions,
  tools: bundle.tools,
  expiresInSeconds: 600,
  safetyIdentifier: hashOf(user.id),
});
```

The client caches the secret until shortly before expiry, so reconnecting does not make a round trip
it does not need.

Azure OpenAI speaks the same event protocol; point `webSocketUrl` at the deployment and supply the
Azure credential.

## Writing another provider

1. Implement `connect` and return a `RealtimeConnection`.
2. Map your engine's events onto `ProviderEvent`. Collect a turn's function calls into one
   `tool.calls`.
3. Declare `capabilities` honestly. Riff uses them to decide what to configure.
4. Translate `SessionDefaults` in one place, the way `session-config.ts` does.

Shapes worth knowing about:

- **Gemini Live** — persistent WebSocket, native speech-to-speech, built-in VAD and barge-in,
  resumable sessions. A close structural match; the differences are session lifecycle events and
  audio encoding.
- **Azure OpenAI Realtime** — the same protocol. A base-URL and auth swap.
- **Chained pipelines** (ElevenLabs, Deepgram plus your own model, or fully on-device with
  `SpeechAnalyzer` and an on-device model) — three legs behind one connection. Latency is higher and
  the adapter has more work to do, but the interface holds: transcripts in, audio out, tool calls
  relayed. An on-device provider is the reason the seam is drawn here rather than higher up.
