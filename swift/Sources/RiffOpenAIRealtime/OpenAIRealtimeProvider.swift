import Foundation
import RiffCore

/// Riff on the OpenAI Realtime API.
///
/// This type is the only place in the Swift package that knows OpenAI's wire format. It implements
/// the provider protocol and nothing more, which is what keeps the agent's behavior — the ledger,
/// the grounding check, the drafts — identical no matter what is generating the speech.
public struct OpenAIRealtimeProvider: RealtimeProvider {
    public let id = "openai-realtime"

    private let credentials: RiffCredentials
    private let model: String?
    private let organization: String?
    private let project: String?
    private let url: URL
    private let urlSession: URLSession
    private let handshakeTimeout: Duration
    /// Lets tests and gateways substitute a transport without a network.
    private let transportFactory: (@Sendable (TransportRequest) throws -> any RiffTransport)?

    public struct TransportRequest: Sendable {
        public var url: URL
        public var model: String
        public var token: String
        public var onMessage: @Sendable (JSONValue) -> Void
        public var onClose: @Sendable (String?) -> Void
        public var onError: @Sendable (Error) -> Void
    }

    public init(
        credentials: RiffCredentials,
        model: String? = nil,
        organization: String? = nil,
        project: String? = nil,
        url: URL = WebSocketTransport.defaultURL,
        urlSession: URLSession = .shared,
        handshakeTimeout: Duration = .seconds(15),
        transportFactory: (@Sendable (TransportRequest) throws -> any RiffTransport)? = nil
    ) {
        self.credentials = credentials
        self.model = model
        self.organization = organization
        self.project = project
        self.url = url
        self.urlSession = urlSession
        self.handshakeTimeout = handshakeTimeout
        self.transportFactory = transportFactory
    }

    public var capabilities: ProviderCapabilities {
        ProviderCapabilities(
            speechToSpeech: true,
            bargeIn: true,
            semanticTurnDetection: true,
            vocabularyBiasing: .prompt,
            inputTranscription: true,
            functionCalling: true,
            inputAudio: AudioFormat(encoding: .pcmS16LE, sampleRate: 24000, channels: 1),
            outputAudio: AudioFormat(encoding: .pcmS16LE, sampleRate: 24000, channels: 1),
            maxSessionSeconds: 3600
        )
    }

    public func connect(_ request: ConnectRequest) async throws -> any RealtimeConnection {
        let model = self.model ?? request.session.model.preferred
        let token = try await credentials.token()

        let box = ConnectionBox(session: request.session)
        let transportRequest = TransportRequest(
            url: url,
            model: model,
            token: token,
            onMessage: { box.handle($0) },
            onClose: { box.finish(reason: $0) },
            onError: { box.fail($0) }
        )

        let transport: any RiffTransport
        if let transportFactory {
            transport = try transportFactory(transportRequest)
        } else {
            let socket = WebSocketTransport(
                url: url,
                model: model,
                token: token,
                organization: organization,
                project: project,
                urlSession: urlSession,
                onMessage: transportRequest.onMessage,
                onClose: transportRequest.onClose,
                onError: transportRequest.onError
            )
            socket.resume()
            transport = socket
        }
        box.attach(transport)

        do {
            try await box.awaitHandshake(timeout: handshakeTimeout)
        } catch {
            await transport.close(reason: "handshake failed")
            throw error
        }

        try transport.send(.object([
            "type": .string(OpenAIClientEvent.sessionUpdate),
            "session": OpenAISessionConfig.build(
                session: request.session,
                instructions: request.instructions,
                tools: request.tools,
                vocabulary: request.vocabulary,
                model: model
            ),
        ]))

        return box
    }
}

/// Holds the connection state shared between the transport callbacks and the session consuming it.
final class ConnectionBox: RealtimeConnection, @unchecked Sendable {
    let events: AsyncStream<ProviderEvent>

    private let continuation: AsyncStream<ProviderEvent>.Continuation
    private let session: SessionDefaults
    private let lock = NSLock()
    private var transport: (any RiffTransport)?
    private var handshake: CheckedContinuation<Void, Error>?
    private var handshakeSettled = false
    private var identity: (sessionId: String, model: String) = ("", "")

    init(session: SessionDefaults) {
        self.session = session
        var continuation: AsyncStream<ProviderEvent>.Continuation!
        self.events = AsyncStream(bufferingPolicy: .unbounded) { continuation = $0 }
        self.continuation = continuation
    }

    var sessionId: String {
        lock.lock(); defer { lock.unlock() }
        return identity.sessionId
    }

    var model: String {
        lock.lock(); defer { lock.unlock() }
        return identity.model
    }

    func attach(_ transport: any RiffTransport) {
        lock.lock(); defer { lock.unlock() }
        self.transport = transport
    }

    func awaitHandshake(timeout: Duration) async throws {
        // A task group would wait for the continuation child even after the timer fired, so a
        // session.created that never arrives would hang connect() instead of timing it out.
        let milliseconds = Int(timeout.components.seconds * 1000 + timeout.components.attoseconds / 1_000_000_000_000_000)

        try await withDeadline(
            milliseconds: milliseconds,
            onTimeout: { RiffError.provider("timed out waiting for session.created") }
        ) { [self] in
            try await withCheckedThrowingContinuation { continuation in
                lock.lock()
                if handshakeSettled {
                    lock.unlock()
                    continuation.resume()
                    return
                }
                handshake = continuation
                lock.unlock()
            }
        }
    }

    func handle(_ raw: JSONValue) {
        let mapped = mapServerEvent(raw)

        if let sessionId = mapped.sessionId {
            lock.lock()
            identity = (sessionId, mapped.model ?? "")
            let waiting = handshake
            handshake = nil
            handshakeSettled = true
            lock.unlock()
            waiting?.resume()
        }

        // A failure during the handshake has to surface as a connect error, not a silent timeout.
        if raw["type"]?.stringValue == OpenAIServerEvent.error {
            lock.lock()
            let waiting = handshakeSettled ? nil : handshake
            if waiting != nil { handshake = nil; handshakeSettled = true }
            lock.unlock()
            waiting?.resume(throwing: RiffError.provider(
                raw["error"]?["message"]?.stringValue ?? "the realtime session was rejected"
            ))
        }

        for event in mapped.events { continuation.yield(event) }
    }

    func fail(_ error: Error) {
        continuation.yield(.failed(ProviderFault(
            code: "transport_error",
            message: String(describing: error),
            retryable: true
        )))
    }

    func finish(reason: String?) {
        lock.lock()
        let waiting = handshakeSettled ? nil : handshake
        if waiting != nil { handshake = nil; handshakeSettled = true }
        lock.unlock()
        waiting?.resume(throwing: RiffError.provider(reason.map { "connection closed: \($0)" } ?? "connection closed"))

        continuation.yield(.closed(reason: reason))
        continuation.finish()
    }

    private func send(_ message: JSONValue) {
        lock.lock()
        let transport = self.transport
        lock.unlock()
        try? transport?.send(message)
    }

    func sendAudio(_ chunk: Data) {
        lock.lock()
        let transport = self.transport
        lock.unlock()
        transport?.sendAudio(chunk)
    }

    func commitAudio() {
        send(.object(["type": .string(OpenAIClientEvent.commitAudio)]))
    }

    func sendText(_ text: String, respond: Bool) {
        send(.object([
            "type": .string(OpenAIClientEvent.createItem),
            "item": .object([
                "type": .string("message"),
                "role": .string("user"),
                "content": .array([.object(["type": .string("input_text"), "text": .string(text)])]),
            ]),
        ]))
        if respond { requestResponse() }
    }

    func respondToTool(callId: String, resultJson: String) {
        send(.object([
            "type": .string(OpenAIClientEvent.createItem),
            "item": .object([
                "type": .string("function_call_output"),
                "call_id": .string(callId),
                "output": .string(resultJson),
            ]),
        ]))
    }

    func requestResponse() {
        send(.object(["type": .string(OpenAIClientEvent.createResponse)]))
    }

    func cancelResponse() {
        send(.object(["type": .string(OpenAIClientEvent.cancelResponse)]))
    }

    func updateVocabulary(_ vocabulary: [String]) {
        guard let patch = OpenAISessionConfig.vocabularyPatch(session: session, vocabulary: vocabulary) else { return }
        send(.object(["type": .string(OpenAIClientEvent.sessionUpdate), "session": patch]))
    }

    func close(reason: String?) async {
        await detachTransport()?.close(reason: reason)
    }

    // NSLock cannot be taken from an async context, so the critical section stays synchronous.
    private func detachTransport() -> (any RiffTransport)? {
        lock.lock(); defer { lock.unlock() }
        let current = transport
        transport = nil
        return current
    }
}
