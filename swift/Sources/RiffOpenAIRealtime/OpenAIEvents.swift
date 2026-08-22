import Foundation
import RiffCore

/// Server event names on the GA interface. The beta names for the same events differ
/// (`response.audio.delta` rather than `response.output_audio.delta`, and so on), and a client that
/// listens for the wrong ones connects successfully and then sits in silence, so both are handled.
public enum OpenAIServerEvent {
    public static let sessionCreated = "session.created"
    public static let speechStarted = "input_audio_buffer.speech_started"
    public static let speechStopped = "input_audio_buffer.speech_stopped"
    public static let transcriptDelta = "conversation.item.input_audio_transcription.delta"
    public static let transcriptCompleted = "conversation.item.input_audio_transcription.completed"
    public static let transcriptFailed = "conversation.item.input_audio_transcription.failed"
    public static let responseCreated = "response.created"
    public static let responseDone = "response.done"
    public static let audioDelta = "response.output_audio.delta"
    public static let audioDone = "response.output_audio.done"
    public static let audioTranscriptDelta = "response.output_audio_transcript.delta"
    public static let audioTranscriptDone = "response.output_audio_transcript.done"
    public static let textDelta = "response.output_text.delta"
    public static let textDone = "response.output_text.done"
    public static let rateLimits = "rate_limits.updated"
    public static let error = "error"

    /// Beta names still emitted by older snapshots, mapped onto their GA equivalents.
    static let legacyAliases = [
        "response.audio.delta": audioDelta,
        "response.audio.done": audioDone,
        "response.audio_transcript.delta": audioTranscriptDelta,
        "response.audio_transcript.done": audioTranscriptDone,
        "response.text.delta": textDelta,
        "response.text.done": textDone,
    ]
}

public enum OpenAIClientEvent {
    public static let sessionUpdate = "session.update"
    public static let appendAudio = "input_audio_buffer.append"
    public static let commitAudio = "input_audio_buffer.commit"
    public static let createItem = "conversation.item.create"
    public static let createResponse = "response.create"
    public static let cancelResponse = "response.cancel"
}

public struct MappedEvents: Sendable {
    public var events: [ProviderEvent]
    /// Session id from `session.created`, which the connection reports as its own identity.
    public var sessionId: String?
    public var model: String?
}

/// Turns one OpenAI server event into zero or more provider events.
///
/// Function calls are read from `response.done` rather than from the streaming argument deltas,
/// because that is the only event guaranteed to carry every call of a turn at once. The session
/// relies on that to know when it has dispatched them all and may ask the model to continue.
public func mapServerEvent(_ raw: JSONValue) -> MappedEvents {
    guard let rawType = raw["type"]?.stringValue else { return MappedEvents(events: []) }
    let type = OpenAIServerEvent.legacyAliases[rawType] ?? rawType
    var events: [ProviderEvent] = []

    switch type {
    case OpenAIServerEvent.sessionCreated:
        let session = raw["session"]
        let sessionId = session?["id"]?.stringValue ?? raw["session_id"]?.stringValue ?? "unknown"
        let model = session?["model"]?.stringValue ?? "unknown"
        return MappedEvents(
            events: [.connected(sessionId: sessionId, model: model)],
            sessionId: sessionId,
            model: model
        )

    case OpenAIServerEvent.speechStarted:
        events.append(.speechStarted)

    case OpenAIServerEvent.speechStopped:
        events.append(.speechStopped)

    case OpenAIServerEvent.transcriptDelta:
        events.append(.transcriptDelta(
            itemId: raw["item_id"]?.stringValue ?? "",
            delta: raw["delta"]?.stringValue ?? ""
        ))

    case OpenAIServerEvent.transcriptCompleted:
        events.append(.transcriptCompleted(
            itemId: raw["item_id"]?.stringValue ?? "",
            text: raw["transcript"]?.stringValue ?? "",
            confidence: averageConfidence(raw["logprobs"])
        ))

    case OpenAIServerEvent.transcriptFailed:
        events.append(.transcriptFailed(
            itemId: raw["item_id"]?.stringValue ?? "",
            reason: raw["error"]?["message"]?.stringValue ?? "transcription failed"
        ))

    case OpenAIServerEvent.responseCreated:
        events.append(.responseStarted(
            responseId: raw["response"]?["id"]?.stringValue ?? raw["response_id"]?.stringValue ?? ""
        ))

    case OpenAIServerEvent.audioDelta:
        if let delta = raw["delta"]?.stringValue, let audio = Data(base64Encoded: delta) {
            events.append(.responseAudio(responseId: raw["response_id"]?.stringValue ?? "", audio: audio))
        }

    case OpenAIServerEvent.audioDone:
        events.append(.responseAudioDone(responseId: raw["response_id"]?.stringValue ?? ""))

    case OpenAIServerEvent.audioTranscriptDelta, OpenAIServerEvent.textDelta:
        events.append(.responseTextDelta(
            responseId: raw["response_id"]?.stringValue ?? "",
            delta: raw["delta"]?.stringValue ?? ""
        ))

    case OpenAIServerEvent.audioTranscriptDone, OpenAIServerEvent.textDone:
        events.append(.responseText(
            responseId: raw["response_id"]?.stringValue ?? "",
            text: raw["transcript"]?.stringValue ?? raw["text"]?.stringValue ?? ""
        ))

    case OpenAIServerEvent.responseDone:
        let response = raw["response"]
        let id = response?["id"]?.stringValue ?? ""
        let status = response?["status"]?.stringValue

        var calls: [ToolCallRequest] = []
        for item in response?["output"]?.arrayValue ?? [] where item["type"]?.stringValue == "function_call" {
            calls.append(ToolCallRequest(
                callId: item["call_id"]?.stringValue ?? "",
                name: item["name"]?.stringValue ?? "",
                argumentsJson: item["arguments"]?.stringValue ?? "{}"
            ))
        }
        if !calls.isEmpty { events.append(.toolCalls(calls)) }

        if status == "cancelled" {
            events.append(.responseCancelled(responseId: id))
        } else if status == "failed" || status == "incomplete" {
            let error = response?["status_details"]?["error"]
            events.append(.failed(ProviderFault(
                code: error?["code"]?.stringValue ?? "response_\(status ?? "failed")",
                message: error?["message"]?.stringValue ?? "response \(status ?? "failed")",
                retryable: true
            )))
            events.append(.responseDone(responseId: id, usage: nil))
        } else {
            events.append(.responseDone(responseId: id, usage: usage(response?["usage"])))
        }

    case OpenAIServerEvent.rateLimits:
        let limits = raw["rate_limits"]?.arrayValue ?? []
        if let tightest = limits.min(by: { ($0["remaining"]?.intValue ?? 0) < ($1["remaining"]?.intValue ?? 0) }) {
            events.append(.rateLimit(
                remaining: tightest["remaining"]?.intValue ?? 0,
                resetSeconds: tightest["reset_seconds"]?.intValue ?? 0
            ))
        }

    case OpenAIServerEvent.error:
        let error = raw["error"] ?? raw
        let code = error["code"]?.stringValue ?? error["type"]?.stringValue ?? "provider_error"
        events.append(.failed(ProviderFault(
            code: code,
            message: error["message"]?.stringValue ?? "the provider reported an error",
            // A rejected session config will be rejected again; a transport hiccup will not.
            retryable: !code.contains("invalid_request")
        )))

    default:
        break
    }

    return MappedEvents(events: events)
}

private func usage(_ value: JSONValue?) -> TokenUsage? {
    guard let value else { return nil }
    let details = value["input_token_details"]
    return TokenUsage(
        inputTokens: value["input_tokens"]?.intValue,
        outputTokens: value["output_tokens"]?.intValue,
        inputAudioTokens: details?["audio_tokens"]?.intValue,
        cachedTokens: details?["cached_tokens"]?.intValue
    )
}

/// Turns transcription logprobs into a rough confidence, used to decide when to double check a word.
private func averageConfidence(_ value: JSONValue?) -> Double? {
    let logprobs = (value?.arrayValue ?? []).compactMap { $0["logprob"]?.numberValue }
    guard !logprobs.isEmpty else { return nil }
    let mean = logprobs.reduce(0, +) / Double(logprobs.count)
    return (exp(mean) * 1000).rounded() / 1000
}
