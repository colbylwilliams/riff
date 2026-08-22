import Foundation

public struct AudioFormat: Sendable, Equatable {
    public enum Encoding: String, Sendable { case pcmS16LE = "pcm_s16le", opus, g711ulaw = "g711_ulaw", g711alaw = "g711_alaw" }
    public var encoding: Encoding
    public var sampleRate: Int
    public var channels: Int

    public init(encoding: Encoding, sampleRate: Int, channels: Int) {
        self.encoding = encoding
        self.sampleRate = sampleRate
        self.channels = channels
    }
}

public struct ProviderCapabilities: Sendable {
    public enum Biasing: String, Sendable { case keywords, prompt, none }

    /// Speech in and speech out through one model, rather than a transcribe-think-speak chain.
    public var speechToSpeech: Bool
    /// The speaker can talk over the agent and cut it off mid-sentence.
    public var bargeIn: Bool
    /// The provider decides when a turn has ended from meaning, not just from silence.
    public var semanticTurnDetection: Bool
    /// Transcription can be biased toward a supplied vocabulary.
    public var vocabularyBiasing: Biasing
    /// Verbatim transcripts of the speaker are available, which the prompt body depends on.
    public var inputTranscription: Bool
    public var functionCalling: Bool
    public var inputAudio: AudioFormat
    public var outputAudio: AudioFormat
    public var maxSessionSeconds: Int?

    public init(
        speechToSpeech: Bool,
        bargeIn: Bool,
        semanticTurnDetection: Bool,
        vocabularyBiasing: Biasing,
        inputTranscription: Bool,
        functionCalling: Bool,
        inputAudio: AudioFormat,
        outputAudio: AudioFormat,
        maxSessionSeconds: Int? = nil
    ) {
        self.speechToSpeech = speechToSpeech
        self.bargeIn = bargeIn
        self.semanticTurnDetection = semanticTurnDetection
        self.vocabularyBiasing = vocabularyBiasing
        self.inputTranscription = inputTranscription
        self.functionCalling = functionCalling
        self.inputAudio = inputAudio
        self.outputAudio = outputAudio
        self.maxSessionSeconds = maxSessionSeconds
    }
}

public struct ConnectRequest: Sendable {
    public var instructions: String
    public var tools: [ToolDefinition]
    public var session: SessionDefaults
    /// Canonical spellings to bias transcription toward, compiled from the active lexicon.
    public var vocabulary: [String]

    public init(instructions: String, tools: [ToolDefinition], session: SessionDefaults, vocabulary: [String] = []) {
        self.instructions = instructions
        self.tools = tools
        self.session = session
        self.vocabulary = vocabulary
    }
}

public struct TokenUsage: Sendable {
    public var inputTokens: Int?
    public var outputTokens: Int?
    public var inputAudioTokens: Int?
    public var cachedTokens: Int?

    public init(inputTokens: Int? = nil, outputTokens: Int? = nil, inputAudioTokens: Int? = nil, cachedTokens: Int? = nil) {
        self.inputTokens = inputTokens
        self.outputTokens = outputTokens
        self.inputAudioTokens = inputAudioTokens
        self.cachedTokens = cachedTokens
    }
}

public struct ProviderFault: Sendable {
    public var code: String
    public var message: String
    /// Whether reconnecting is worth trying. Transport faults are; a rejected session config is not.
    public var retryable: Bool

    public init(code: String, message: String, retryable: Bool) {
        self.code = code
        self.message = message
        self.retryable = retryable
    }
}

public struct ToolCallRequest: Sendable {
    public var callId: String
    public var name: String
    public var argumentsJson: String

    public init(callId: String, name: String, argumentsJson: String) {
        self.callId = callId
        self.name = name
        self.argumentsJson = argumentsJson
    }
}

public enum ProviderEvent: Sendable {
    case connected(sessionId: String, model: String)
    case speechStarted
    case speechStopped
    case transcriptDelta(itemId: String, delta: String)
    case transcriptCompleted(itemId: String, text: String, confidence: Double?)
    case transcriptFailed(itemId: String, reason: String)
    case responseStarted(responseId: String)
    case responseAudio(responseId: String, audio: Data)
    case responseAudioDone(responseId: String)
    case responseTextDelta(responseId: String, delta: String)
    case responseText(responseId: String, text: String)
    case responseDone(responseId: String, usage: TokenUsage?)
    case responseCancelled(responseId: String)
    case toolCalls([ToolCallRequest])
    case rateLimit(remaining: Int, resetSeconds: Int)
    case failed(ProviderFault)
    case closed(reason: String?)
}

/// The seam between Riff and whatever produces speech-to-speech.
///
/// It is deliberately small. Everything that makes Riff what it is — the ledger, the grounding
/// check, drafts, takes, the artifact — lives above this line and is provider independent. A
/// provider only has to move audio, report what was heard, and relay tool calls.
public protocol RealtimeProvider: Sendable {
    var id: String { get }
    var capabilities: ProviderCapabilities { get }
    func connect(_ request: ConnectRequest) async throws -> any RealtimeConnection
}

public protocol RealtimeConnection: Sendable {
    var sessionId: String { get }
    var model: String { get }
    /// Events from the provider. Consumed by the session, which is the only subscriber.
    var events: AsyncStream<ProviderEvent> { get }

    /// Appends captured audio in the format the provider advertised.
    func sendAudio(_ chunk: Data)
    /// Ends the current turn explicitly. Only needed when turn detection is manual.
    func commitAudio()
    /// Injects text as if the speaker had said it, for typed input and host notifications.
    func sendText(_ text: String, respond: Bool)
    /// Returns a tool result. `resultJson` is a JSON string, matching every provider's expectation.
    func respondToTool(callId: String, resultJson: String)
    func requestResponse()
    /// Stops the agent mid-sentence. Used when the speaker talks over it.
    func cancelResponse()
    /// Pushes newly learned vocabulary without resending the whole configuration.
    func updateVocabulary(_ vocabulary: [String])
    func close(reason: String?) async
}
