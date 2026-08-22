import Foundation

public enum SessionState: String, Sendable {
    case idle, connecting, listening, thinking, speaking, closing, closed, failed
}

public enum RiffEvent: Sendable {
    case state(SessionState, previous: SessionState)
    case utterance(Utterance)
    case draft(takeId: String, ready: Bool, fidelity: Double, gist: String)
    case take(String?)
    case agentTranscript(String, final: Bool)
    case agentAudio(Data)
    case interrupted
    case tool(name: String, ok: Bool, durationMs: Int)
    case submitted(PromptArtifact)
    case expiring(secondsRemaining: Int)
    case failed(ProviderFault)
    case closed(reason: String?)
}

public struct SessionOverrides: Sendable {
    public var model: String?
    public var voice: String?
    public var speed: Double?
    public var language: String?
    public var transcriptionModel: String?
    public var maxOutputTokens: Int?
    public var reasoningEffort: String?

    public init(
        model: String? = nil,
        voice: String? = nil,
        speed: Double? = nil,
        language: String? = nil,
        transcriptionModel: String? = nil,
        maxOutputTokens: Int? = nil,
        reasoningEffort: String? = nil
    ) {
        self.model = model
        self.voice = voice
        self.speed = speed
        self.language = language
        self.transcriptionModel = transcriptionModel
        self.maxOutputTokens = maxOutputTokens
        self.reasoningEffort = reasoningEffort
    }

    /// Applies these overrides to the bundle's session defaults.
    public func apply(to defaults: SessionDefaults) -> SessionDefaults {
        var session = defaults
        if let model { session.model.preferred = model }
        if let voice { session.voice.name = voice }
        if let speed { session.voice.speed = speed }
        if let language { session.transcription.language = language }
        if let transcriptionModel { session.transcription.preferred = transcriptionModel }
        if let maxOutputTokens { session.limits.maxOutputTokens = maxOutputTokens }
        if let reasoningEffort { session.reasoning = SessionDefaults.Reasoning(effort: reasoningEffort) }
        return session
    }
}

/// One conversation, from the first word to a submitted prompt.
///
/// The session owns the pieces that have to agree with each other: what was heard, what has been
/// drafted from it, and what the agent is allowed to do next. Provider events flow in, tool calls
/// flow back out, and the ledger stays the single place the prompt body can come from — which is
/// what makes "in their own words" a property of the system rather than a request in a prompt.
@MainActor
public final class RiffSession {
    public let bundle: AgentBundle
    public let lexicon: Lexicon
    public let ledger: UtteranceLedger
    public let book: DraftBook

    public private(set) var state: SessionState = .idle

    /// Everything the embedding application needs to render, in order.
    public let events: AsyncStream<RiffEvent>

    private let provider: any RealtimeProvider
    private let host: any RiffHost
    private let store: any RiffStore
    private let overrides: SessionOverrides
    private let continuation: AsyncStream<RiffEvent>.Continuation

    private var connection: (any RealtimeConnection)?
    private var runtime: ToolRuntime?
    private var registry: ToolRegistry?
    private var pump: Task<Void, Never>?
    private var expiryTimers: [Task<Void, Never>] = []
    private var toolsInFlight = false
    private var agentTranscript = ""
    private var startedAt: Date?
    private var toolLog: [PromptArtifact.ToolCall] = []

    public init(
        bundle: AgentBundle,
        provider: any RealtimeProvider,
        host: any RiffHost = NullHost(),
        store: any RiffStore = MemoryStore(),
        overrides: SessionOverrides = SessionOverrides()
    ) {
        self.bundle = bundle
        self.provider = provider
        self.host = host
        self.store = store
        self.overrides = overrides

        let lexicon = Lexicon(bundle.lexicon.terms)
        self.lexicon = lexicon
        self.ledger = UtteranceLedger(
            lexicon: lexicon,
            windowSize: bundle.grounding.windowSize,
            redact: bundle.policy.redactSecretsFromTranscript != false
        )
        self.book = DraftBook(policy: bundle.policy)

        var continuation: AsyncStream<RiffEvent>.Continuation!
        self.events = AsyncStream { continuation = $0 }
        self.continuation = continuation
    }

    public var sessionId: String? { connection?.sessionId }

    public func start() async throws {
        guard state == .idle || state == .closed || state == .failed else {
            throw RiffError.provider("session already \(state.rawValue)")
        }
        setState(.connecting)

        do {
            for term in try await store.loadLexicon() { lexicon.add(term) }
            var motifs: [String: Motif] = [:]
            for motif in try await store.listMotifs() { motifs[motif.id] = motif }

            let environment = (try? await host.environment()) ?? HostEnvironment()
            for term in environment.vocabulary { lexicon.add(term) }
            ledger.invalidate()

            let runtime = ToolRuntime(
                bundle: bundle,
                ledger: ledger,
                book: book,
                lexicon: lexicon,
                checker: GroundingChecker(config: bundle.grounding, lexicon: lexicon),
                host: host,
                store: store
            )
            runtime.motifs = motifs
            runtime.provenance = { [weak self] in self?.provenance() ?? PromptArtifact.Provenance(fidelity: 0, utteranceCount: 0, bodyTokens: 0, agentAuthoredTokens: 0) }
            runtime.onLexiconChanged = { [weak self] in
                guard let self else { return }
                self.connection?.updateVocabulary(self.vocabulary())
            }
            runtime.onDraftChanged = { [weak self] take in self?.emitDraft(take) }
            runtime.onTakeChanged = { [weak self] take in self?.emit(.take(take?.id)) }
            runtime.onSubmitted = { [weak self] artifact in self?.emit(.submitted(artifact)) }

            self.runtime = runtime
            self.registry = ToolRegistry(runtime: runtime)

            let session = overrides.apply(to: bundle.session)
            let connection = try await provider.connect(ConnectRequest(
                instructions: bundle.instructions,
                tools: bundle.tools,
                session: session,
                vocabulary: vocabulary()
            ))
            self.connection = connection
            self.startedAt = Date()

            pump = Task { [weak self] in
                for await event in connection.events {
                    guard let self else {
                        // The session was released while connected. Tear the connection down rather
                        // than leaving a live microphone attached to an object nobody can reach.
                        await connection.close(reason: "session released")
                        return
                    }
                    await self.handle(event)
                }
            }

            scheduleExpiryWarnings(maxSessionSeconds: session.limits.maxSessionSeconds)

            if let note = describeEnvironment(environment) {
                connection.sendText(note, respond: false)
            }

            setState(.listening)
        } catch {
            setState(.failed)
            emit(.failed(ProviderFault(code: "connect_failed", message: String(describing: error), retryable: true)))
            throw error
        }
    }

    public func stop(reason: String = "ended") async {
        guard state != .closed, state != .idle else { return }
        setState(.closing)
        for timer in expiryTimers { timer.cancel() }
        expiryTimers = []
        pump?.cancel()
        pump = nil
        await connection?.close(reason: reason)
        connection = nil
        setState(.closed)
        emit(.closed(reason: reason))
        // The stream is deliberately left open. `start()` permits a restart, and finishing here
        // would leave a restarted session connected but emitting nothing. It ends when the session
        // is released; `.closed` is the signal for a consumer to stop iterating.
    }

    /// Captured microphone audio, in the format the provider advertised.
    public func send(audio: Data) {
        connection?.sendAudio(audio)
    }

    /// Typed input, treated exactly like speech: it lands in the ledger and can be quoted in the
    /// prompt. Someone switching to the keyboard mid-thought should not lose the ability to be quoted.
    @discardableResult
    public func send(text: String) throws -> Utterance {
        guard let connection else { throw RiffError.notConnected }
        let utterance = ledger.append(text: text, source: .typed)
        emit(.utterance(utterance))
        connection.sendText(utterance.text, respond: true)
        return utterance
    }

    /// Cuts the agent off. Called when the speaker starts talking over it.
    public func interrupt() {
        guard state == .speaking || state == .thinking else { return }
        connection?.cancelResponse()
        emit(.interrupted)
        setState(.listening)
    }

    /// The active take as it stands right now. Always safe to read mid-conversation.
    public func artifact() throws -> PromptArtifact? {
        guard let id = book.activeId, let take = book.take(id) else { return nil }
        return try buildArtifact(take, options: BuildArtifactOptions(
            render: RenderOptions(config: bundle.render),
            lexicon: lexicon,
            utteranceCount: ledger.count,
            provenance: provenance()
        ))
    }

    // MARK: - Provider events

    private func handle(_ event: ProviderEvent) async {
        switch event {
        case .connected:
            setState(.listening)

        case .speechStarted:
            if state == .speaking || state == .thinking { emit(.interrupted) }
            setState(.listening)

        case .transcriptCompleted(_, let text, let confidence):
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty else { return }
            emit(.utterance(ledger.append(text: trimmed, source: .speech, confidence: confidence)))

        case .transcriptFailed(_, let reason):
            emit(.failed(ProviderFault(code: "transcription_failed", message: reason, retryable: true)))

        case .responseStarted:
            // The continuation for a tool batch has begun, so the batch is no longer pending.
            toolsInFlight = false
            agentTranscript = ""
            setState(.thinking)

        case .responseAudio(_, let audio):
            if state != .speaking { setState(.speaking) }
            emit(.agentAudio(audio))

        case .responseTextDelta(_, let delta):
            agentTranscript += delta
            emit(.agentTranscript(agentTranscript, final: false))

        case .responseText(_, let text):
            agentTranscript = text
            emit(.agentTranscript(text, final: true))

        case .toolCalls(let calls):
            // Set on receipt rather than inside the dispatch: the pump awaits `runTools`, so a flag
            // scoped to that call is already cleared by the time `.responseDone` is read.
            toolsInFlight = true
            await runTools(calls)

        case .responseDone, .responseCancelled:
            if !toolsInFlight { setState(.listening) }

        case .failed(let fault):
            toolsInFlight = false
            emit(.failed(fault))
            if !fault.retryable { setState(.failed) }

        case .closed(let reason):
            setState(.closed)
            emit(.closed(reason: reason))

        case .speechStopped, .responseAudioDone, .rateLimit, .transcriptDelta:
            break
        }
    }

    /// Runs every tool call of one model turn, then asks for exactly one continuation.
    ///
    /// Providers deliver a turn's calls together, so they are dispatched together. Asking the model
    /// to continue once per call instead would produce one spoken reply per tool, which sounds like
    /// the agent stuttering.
    private func runTools(_ calls: [ToolCallRequest]) async {
        guard let registry, let connection, !calls.isEmpty else {
            toolsInFlight = false
            return
        }

        for call in calls {
            let at = ISO8601.now()
            let outcome = await registry.dispatch(name: call.name, argumentsJson: call.argumentsJson)

            toolLog.append(PromptArtifact.ToolCall(
                name: call.name,
                at: at,
                durationMs: outcome.durationMs,
                ok: outcome.ok
            ))
            emit(.tool(name: call.name, ok: outcome.ok, durationMs: outcome.durationMs))

            connection.respondToTool(callId: call.callId, resultJson: outcome.result.serialized())
        }

        connection.requestResponse()
    }

    // MARK: - Internals

    private func provenance() -> PromptArtifact.Provenance {
        PromptArtifact.Provenance(
            fidelity: 0,
            utteranceCount: ledger.count,
            bodyTokens: 0,
            agentAuthoredTokens: 0,
            agentVersion: bundle.version,
            bundleRevision: bundle.revision,
            providerId: provider.id,
            model: connection?.model,
            sessionId: connection?.sessionId,
            durationMs: startedAt.map { Int(Date().timeIntervalSince($0) * 1000) },
            toolCalls: toolLog
        )
    }

    private func vocabulary() -> [String] {
        guard let biasing = bundle.session.transcription.biasing, biasing.enabled else { return [] }
        return lexicon.keywords(limit: biasing.maxKeywords)
    }

    private func emitDraft(_ take: Take) {
        guard let artifact = try? buildArtifact(take, options: BuildArtifactOptions(
            render: RenderOptions(config: bundle.render),
            lexicon: lexicon,
            utteranceCount: ledger.count
        )) else { return }

        emit(.draft(
            takeId: take.id,
            ready: take.isReady(policy: bundle.policy),
            fidelity: artifact.provenance.fidelity,
            gist: summarizeDraft(take)
        ))
    }

    private func scheduleExpiryWarnings(maxSessionSeconds: Int) {
        for remaining in [300, 60] {
            let delay = maxSessionSeconds - remaining
            guard delay > 0 else { continue }
            expiryTimers.append(Task { [weak self] in
                try? await Task.sleep(for: .seconds(delay))
                guard !Task.isCancelled else { return }
                self?.emit(.expiring(secondsRemaining: remaining))
            })
        }
    }

    private func setState(_ next: SessionState) {
        guard next != state else { return }
        let previous = state
        state = next
        emit(.state(next, previous: previous))
    }

    private func emit(_ event: RiffEvent) {
        continuation.yield(event)
    }
}

/// Ambient facts stated once at connect time so references resolve without anyone being asked.
///
/// This is injected into the model's context, not into the ledger, so none of it can end up quoted
/// in the prompt as though the speaker had said it.
func describeEnvironment(_ environment: HostEnvironment) -> String? {
    var facts: [String] = []
    if let repository = environment.repository { facts.append("Repository: \(repository)") }
    if let branch = environment.branch { facts.append("Branch: \(branch)") }
    if let workspace = environment.workspace { facts.append("Workspace: \(workspace)") }
    if let login = environment.userLogin { facts.append("Speaker: \(environment.userName ?? login)") }
    if !environment.destinations.isEmpty {
        let listed = environment.destinations
            .map { "\($0.id)\($0.isDefault ? " (default)" : "")" }
            .joined(separator: ", ")
        facts.append("Destinations: \(listed)")
    }
    if !environment.recent.isEmpty {
        let listed = environment.recent.prefix(5).map { $0.identifier ?? $0.title }.joined(separator: ", ")
        facts.append("Recently touched: \(listed)")
    }

    guard !facts.isEmpty else { return nil }
    return "[context, not spoken by the user, never quote this in the prompt]\n" + facts.joined(separator: "\n")
}
