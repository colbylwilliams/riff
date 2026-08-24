import Foundation
import Testing
@testable import RiffCore
@testable import RiffOpenAIRealtime

/// A provider the test drives by hand, standing in for a speech-to-speech model.
final class FakeConnection: RealtimeConnection, @unchecked Sendable {
    enum Call: Sendable, Equatable {
        case audio(Int)
        case commit
        case text(String, respond: Bool)
        case toolResult(callId: String, json: String)
        case requestResponse
        case cancel
        case vocabulary([String])
        case close
    }

    let sessionId = "sess_fake"
    let model = "fake-realtime"
    let events: AsyncStream<ProviderEvent>

    private let continuation: AsyncStream<ProviderEvent>.Continuation
    private let lock = NSLock()
    private var recorded: [Call] = []
    private var finished = false

    init() {
        var continuation: AsyncStream<ProviderEvent>.Continuation!
        events = AsyncStream(bufferingPolicy: .unbounded) { continuation = $0 }
        self.continuation = continuation
    }

    var calls: [Call] {
        lock.lock(); defer { lock.unlock() }
        return recorded
    }

    private func record(_ call: Call) {
        lock.lock(); recorded.append(call); lock.unlock()
    }

    func emit(_ event: ProviderEvent) { continuation.yield(event) }

    /// Simulates a completed turn of speech arriving from transcription.
    func say(_ text: String) {
        emit(.transcriptCompleted(itemId: "item_\(UUID().uuidString.prefix(6))", text: text, confidence: nil))
    }

    /// Simulates the model calling one tool in a turn.
    @discardableResult
    func callTool(_ name: String, _ arguments: JSONValue) -> String {
        callTools([(name, arguments)])[0]
    }

    /// Simulates a turn in which the model calls several tools at once.
    @discardableResult
    func callTools(_ calls: [(name: String, arguments: JSONValue)]) -> [String] {
        let requests = calls.map { call in
            ToolCallRequest(
                callId: "call_\(UUID().uuidString.prefix(6))",
                name: call.name,
                argumentsJson: call.arguments.serialized()
            )
        }
        emit(.toolCalls(requests))
        return requests.map(\.callId)
    }

    /// The JSON result handed back for a given tool call, once the session has dispatched it.
    /// Polls rather than sleeping a fixed time, because tool handlers await the host and the store.
    func result(for callId: String, timeout: Duration = .seconds(5)) async throws -> JSONValue {
        let deadline = ContinuousClock.now.advanced(by: timeout)
        repeat {
            for call in calls.reversed() {
                if case .toolResult(let id, let json) = call, id == callId {
                    return try JSONValue.parse(json)
                }
            }
            await Task.yield()
            try? await Task.sleep(for: .milliseconds(2))
        } while ContinuousClock.now < deadline
        throw RiffError.tool("no tool result was sent for \(callId)")
    }

    /// Waits for a recorded call matching a predicate.
    func waitForCall(timeout: Duration = .seconds(5), where matches: @escaping @Sendable (Call) -> Bool) async -> Bool {
        let deadline = ContinuousClock.now.advanced(by: timeout)
        repeat {
            if calls.contains(where: matches) { return true }
            await Task.yield()
            try? await Task.sleep(for: .milliseconds(2))
        } while ContinuousClock.now < deadline
        return false
    }

    func sendAudio(_ chunk: Data) { record(.audio(chunk.count)) }
    func commitAudio() { record(.commit) }
    func sendText(_ text: String, respond: Bool) { record(.text(text, respond: respond)) }
    func respondToTool(callId: String, resultJson: String) { record(.toolResult(callId: callId, json: resultJson)) }
    func requestResponse() { record(.requestResponse) }
    func cancelResponse() { record(.cancel) }
    func updateVocabulary(_ vocabulary: [String]) { record(.vocabulary(vocabulary)) }
    func close(reason: String?) async {
        record(.close)
        // NSLock cannot be taken from an async context, so the critical section stays synchronous.
        markFinished()
        continuation.finish()
    }

    private func markFinished() {
        lock.lock(); finished = true; lock.unlock()
    }

    var isFinished: Bool {
        lock.lock(); defer { lock.unlock() }
        return finished
    }
}

final class FakeProvider: RealtimeProvider, @unchecked Sendable {
    let id = "fake"
    private let lock = NSLock()
    private var recorded: ConnectRequest?
    // A fresh connection per connect, as a real provider gives: a closed one's event stream is done.
    private var current = FakeConnection()

    var connection: FakeConnection {
        lock.lock(); defer { lock.unlock() }
        return current
    }

    var capabilities: ProviderCapabilities {
        ProviderCapabilities(
            speechToSpeech: true,
            bargeIn: true,
            semanticTurnDetection: true,
            vocabularyBiasing: .keywords,
            inputTranscription: true,
            functionCalling: true,
            inputAudio: AudioFormat(encoding: .pcmS16LE, sampleRate: 24000, channels: 1),
            outputAudio: AudioFormat(encoding: .pcmS16LE, sampleRate: 24000, channels: 1)
        )
    }

    var request: ConnectRequest? {
        lock.lock(); defer { lock.unlock() }
        return recorded
    }

    func connect(_ request: ConnectRequest) async throws -> any RealtimeConnection {
        // NSLock cannot be taken from an async context, so the critical section stays synchronous.
        store(request)
    }


    private func store(_ request: ConnectRequest) -> FakeConnection {
        lock.lock(); defer { lock.unlock() }
        recorded = request
        if current.isFinished { current = FakeConnection() }
        return current
    }
}

final class RecordingHost: RiffHost, @unchecked Sendable {
    private let lock = NSLock()
    private var stored: [ContextItem] = []
    private var received: [PromptArtifact] = []
    private var known: [String] = []
    private var holds = false
    private var stallsLookup = false
    private var submitGate: CheckedContinuation<Void, Never>?

    /// Vocabulary this world knows about, which is what corroborates a spelling correction.
    var knownTerms: [String] {
        get { lock.lock(); defer { lock.unlock() }; return known }
        set { lock.lock(); known = newValue; lock.unlock() }
    }

    var candidates: [ContextItem] {
        get { lock.lock(); defer { lock.unlock() }; return stored }
        set { lock.lock(); stored = newValue; lock.unlock() }
    }

    var submitted: [PromptArtifact] {
        lock.lock(); defer { lock.unlock() }
        return received
    }

    /// Holds `submitPrompt` until released, so a test can let a call that has already timed out
    /// finish afterwards — which is what `withDeadline` leaves behind when it abandons work.
    var holdSubmit: Bool {
        get { lock.lock(); defer { lock.unlock() }; return holds }
        set { lock.lock(); holds = newValue; lock.unlock() }
    }

    /// Makes `resolveReference` never answer, so a second call can still be in flight while an
    /// abandoned first one finishes underneath it.
    var stallsResolve: Bool {
        get { lock.lock(); defer { lock.unlock() }; return stallsLookup }
        set { lock.lock(); stallsLookup = newValue; lock.unlock() }
    }

    /// Lets a held `submitPrompt` return. A gate never released simply never returns.
    func releaseSubmit() {
        takeGate()?.resume()
    }

    private func storeGate(_ gate: CheckedContinuation<Void, Never>) {
        lock.lock(); submitGate = gate; lock.unlock()
    }

    private func takeGate() -> CheckedContinuation<Void, Never>? {
        lock.lock(); defer { lock.unlock() }
        let gate = submitGate
        submitGate = nil
        return gate
    }

    func resolveReference(_ request: ResolveReferenceRequest) async throws -> [ContextItem] {
        if stallsResolve {
            await withCheckedContinuation { (_: CheckedContinuation<Void, Never>) in }
        }
        return candidates
    }
    func lookupTerm(_ request: LookupTermRequest) async throws -> [TermMatch] {
        knownTerms
            .filter { $0.lowercased() == request.heard.lowercased() }
            .map { TermMatch(term: LexiconTerm(canonical: $0, kind: "product"), confidence: 1) }
    }
    func recallPrompts(_ request: RecallPromptsRequest) async throws -> [PriorPrompt] { [] }

    func submitPrompt(_ artifact: PromptArtifact, options: SubmitOptions) async throws -> SubmitResult {
        if holdSubmit {
            // A checked continuation is indifferent to cancellation, which is the point: this models
            // a host that keeps going after Riff has stopped waiting for it.
            await withCheckedContinuation { (gate: CheckedContinuation<Void, Never>) in storeGate(gate) }
        }
        // NSLock cannot be taken from an async context, so the critical section stays synchronous.
        store(artifact)
        return SubmitResult(submitted: true, promptId: "p1", destination: "test", url: "https://example.test/p1")
    }

    private func store(_ artifact: PromptArtifact) {
        lock.lock(); received.append(artifact); lock.unlock()
    }
}

/// Collects session events from a consumer task so a test can wait for one.
final class EventBox: @unchecked Sendable {
    private let lock = NSLock()
    private var texts: [String] = []

    func record(_ text: String) {
        lock.lock(); texts.append(text); lock.unlock()
    }

    private func contains(_ needle: String) -> Bool {
        lock.lock(); defer { lock.unlock() }
        return texts.contains { $0.contains(needle) }
    }

    func wait(for needle: String, timeout: Duration = .seconds(5)) async -> Bool {
        let deadline = ContinuousClock.now.advanced(by: timeout)
        repeat {
            if contains(needle) { return true }
            await Task.yield()
            try? await Task.sleep(for: .milliseconds(2))
        } while ContinuousClock.now < deadline
        return false
    }
}

/// Lets the session's event pump drain queued provider events.
func settle(_ rounds: Int = 8) async {
    for _ in 0..<rounds {
        await Task.yield()
        try? await Task.sleep(for: .milliseconds(2))
    }
}

/// A store whose `saveArtifact` never returns, so the handler is abandoned at its deadline after the
/// host has already taken the prompt.
final class StallingStore: RiffStore, @unchecked Sendable {
    func loadLexicon() async throws -> [LexiconTerm] { [] }
    func saveTerm(_ term: LexiconTerm) async throws {}
    func listMotifs() async throws -> [Motif] { [] }
    func saveMotif(_ motif: Motif) async throws {}
    func retireMotif(id: String, at: String) async throws {}
    func listArtifacts(limit: Int) async throws -> [PromptArtifact] { [] }

    func saveArtifact(_ artifact: PromptArtifact) async throws {
        // Never returns, and ignores cancellation, which is the harsher case: the handler cannot
        // finish however politely it is asked to.
        await withCheckedContinuation { (_: CheckedContinuation<Void, Never>) in }
    }
}

@MainActor
@Suite(.serialized)
struct SessionTests {
    func makeSession() async throws -> (RiffSession, FakeProvider, RecordingHost, MemoryStore) {
        let provider = FakeProvider()
        let host = RecordingHost()
        let store = MemoryStore()
        let session = RiffSession(
            bundle: try AgentBundle.bundled(),
            provider: provider,
            host: host,
            store: store
        )
        try await session.start()
        await settle()
        return (session, provider, host, store)
    }

    @Test("hands the model the composed instructions, the tools, and the vocabulary")
    func connectRequest() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        let request = try #require(provider.request)

        #expect(request.instructions.contains("You are Riff"))
        #expect(request.tools.count == 9)
        #expect(request.vocabulary.contains("GitHub"))
    }

    @Test("records what was said and lets it be drafted verbatim")
    func verbatimCapture() async throws {
        let (session, provider, _, _) = try await makeSession()
        provider.connection.say("the export button does nothing when you have more than a thousand rows")
        await settle()

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object(["op": .string("set_title"), "text": .string("Fix the export button")]),
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing when you have more than a thousand rows"),
                ]),
            ]),
        ]))
        await settle()

        let result = try await provider.connection.result(for: callId)
        #expect(result["rejected"]?.arrayValue?.isEmpty == true)
        #expect(result["fidelity"]?.numberValue == 1)
        #expect(result["draft"]?["ready"]?.boolValue == true)

        let artifact = try #require(try session.artifact())
        #expect(artifact.provenance.fidelity == 1)
        #expect(artifact.lines.first?.grounding.kind == .verbatim)
        #expect(artifact.rendered.contains("# Fix the export button"))
    }

    @Test("rejects a paraphrase and tells the model which words it invented")
    func paraphraseRejected() async throws {
        let (session, provider, _, _) = try await makeSession()
        provider.connection.say("the export button does nothing when you have a ton of rows")
        await settle()

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("The CSV export functionality fails silently for large result sets"),
                ]),
            ]),
        ]))
        await settle()

        let result = try await provider.connection.result(for: callId)
        let rejected = try #require(result["rejected"]?.arrayValue?.first)
        #expect(result["accepted"]?.arrayValue?.isEmpty == true)
        #expect(rejected["reason"]?.stringValue?.contains("they did not say") == true)
        #expect(rejected["unmatched_tokens"]?.arrayValue?.contains(.string("csv")) == true)
        #expect(try session.artifact()?.lines.isEmpty == true)
    }

    @Test("attaches a resolved reference without touching what they said")
    func attachesContext() async throws {
        let (session, provider, host, _) = try await makeSession()
        host.candidates = [ContextItem(
            referenceId: "pr-412",
            kind: "pull_request",
            title: "Chunked uploads",
            identifier: "acme/web#412",
            url: "https://github.com/acme/web/pull/412",
            state: "open"
        )]

        provider.connection.say("take another look at the PR I just opened")
        await settle()

        let resolveCall = provider.connection.callTool("resolve_reference", .object([
            "phrase": .string("the PR I just opened"),
            "kind": .string("pull_request"),
            "recency": .string("latest"),
        ]))
        _ = try await provider.connection.result(for: resolveCall)

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("take another look at the PR I just opened"),
                ]),
                .object(["op": .string("attach_context"), "reference_id": .string("pr-412")]),
            ]),
        ]))
        await settle()

        #expect(try await provider.connection.result(for: callId)["rejected"]?.arrayValue?.isEmpty == true)

        let artifact = try #require(try session.artifact())
        #expect(artifact.context.first?.identifier == "acme/web#412")
        #expect(artifact.context.first?.resolvedFrom == "the PR I just opened")
        #expect(artifact.rendered.contains("acme/web#412"))
        #expect(artifact.rendered.contains("take another look at the PR I just opened"))
    }

    @Test("will not save a motif the agent made up")
    func motifMustBeTheirWording() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        provider.connection.say("keep the diffs small")
        await settle()

        let callId = provider.connection.callTool("motifs", .object([
            "action": .string("save"),
            "text": .string("adhere to conventional commit standards"),
        ]))
        await settle()

        let result = try await provider.connection.result(for: callId)
        #expect(result["saved"]?.boolValue == false)
        #expect(result["reason"]?.stringValue?.contains("their wording") == true)
    }

    @Test("submits only what was captured, with provenance attached")
    func submits() async throws {
        let (session, provider, host, store) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()

        provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object(["op": .string("set_title"), "text": .string("Fix the export button")]),
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        await settle()

        let callId = provider.connection.callTool("submit_prompt", .object([:]))
        await settle()

        #expect(try await provider.connection.result(for: callId)["submitted"]?.boolValue == true)
        #expect(host.submitted.count == 1)

        let artifact = try #require(host.submitted.first)
        #expect(artifact.provenance.fidelity == 1)
        #expect(artifact.provenance.agentAuthoredTokens == 0)
        #expect(artifact.provenance.providerId == "fake")
        #expect(artifact.provenance.toolCalls?.contains { $0.name == "draft_update" } == true)
        #expect(try await store.listArtifacts(limit: 10).count == 1)
    }

    @Test("starts a fresh take after submitting, so nothing lands in a prompt already sent")
    func freshTakeAfterSubmit() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)
        let submittedTakeId = try #require(session.book.activeId)

        let submitted = provider.connection.callTool("submit_prompt", .object([:]))
        _ = try await provider.connection.result(for: submitted)

        provider.connection.say("also the avatars flicker on every scroll")
        await settle()
        let again = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the avatars flicker on every scroll"),
                ]),
            ]),
        ]))

        let draft = try await provider.connection.result(for: again)["draft"]
        #expect(draft?["take_id"]?.stringValue != submittedTakeId, "a submitted take must not keep receiving lines")
        #expect(draft?["sections"]?["intent"]?.arrayValue?.count == 1)
        #expect(session.book.take(submittedTakeId)?.status == .submitted)
    }

    @Test("parks a take and starts a new one when the subject changes")
    func newTakeParksTheOutgoingOne() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("the export button is broken")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button is broken"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)
        let firstTake = try #require(session.book.activeId)

        let created = provider.connection.callTool("takes", .object([
            "action": .string("new"), "label": .string("avatars"),
        ]))
        let secondTake = try #require(try await provider.connection.result(for: created)["take_id"]?.stringValue)

        #expect(firstTake != secondTake)
        // `60-corrections` tells the speaker they can come back to the parked one, so starting a
        // take has to leave the outgoing one parked rather than still marked as being drafted.
        #expect(session.book.take(firstTake)?.status == .parked)
        #expect(session.book.take(secondTake)?.status == .drafting)

        // Both prompts are open at once, and the parked one is readable without becoming active.
        let parked = try session.artifact(takeId: firstTake)
        #expect(parked?.takeId == firstTake)
        #expect(parked?.lines.count == 1)
        #expect(try session.artifact()?.takeId == secondTake)
        #expect(try session.artifact()?.lines.isEmpty == true)

        let switched = provider.connection.callTool("takes", .object([
            "action": .string("switch"), "take_id": .string(firstTake),
        ]))
        let draft = try await provider.connection.result(for: switched)["draft"]
        #expect(draft?["sections"]?["intent"]?.arrayValue?.count == 1)
        #expect(session.book.take(firstTake)?.status == .drafting)
        #expect(session.book.take(secondTake)?.status == .parked)
    }

    @Test("reports a delivered prompt even when saving a copy never returns")
    func hungStoreDoesNotBecomeATimeout() async throws {
        let provider = FakeProvider()
        let host = RecordingHost()
        let session = RiffSession(
            bundle: try AgentBundle.bundled(),
            provider: provider,
            host: host,
            store: StallingStore()
        )
        defer { withExtendedLifetime(session) {} }
        try await session.start()
        await settle()

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)
        let takeId = try #require(session.book.activeId)

        let submitted = provider.connection.callTool("submit_prompt", .object([:]))
        // Longer than the bundle's tool timeout, so the deadline fires on the save that never lands.
        let result = try await provider.connection.result(for: submitted, timeout: .seconds(12))

        // The prompt was delivered, so that is what the model is told — reporting a timeout here is
        // what makes it send a second time.
        #expect(result["submitted"]?.boolValue == true)
        #expect(result["error"] == nil)
        #expect(host.submitted.count == 1)

        // Recorded already, without waiting for the save that will never land.
        #expect(session.book.take(takeId)?.status == .submitted)
        #expect(session.book.activeId == nil)

        provider.connection.say("also the avatars flicker on every scroll")
        await settle()
        let again = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the avatars flicker on every scroll"),
                ]),
            ]),
        ]))
        let draft = try await provider.connection.result(for: again)["draft"]
        #expect(draft?["take_id"]?.stringValue != takeId, "the next line lands in a new prompt")
    }

    @Test("still reports a send that a host never confirms as a timeout")
    func unconfirmedSendIsATimeout() async throws {
        // The other half of the same rule: nothing outside Riff changed, so there is nothing
        // committed and giving up is the honest answer.
        let (session, provider, host, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        host.holdSubmit = true

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)
        let takeId = try #require(session.book.activeId)

        let submitted = provider.connection.callTool("submit_prompt", .object([:]))
        let result = try await provider.connection.result(for: submitted, timeout: .seconds(12))

        #expect(result["error"]?.stringValue?.contains("did not answer") == true)
        #expect(session.book.take(takeId)?.status == .drafting, "an unsent take is left where it was")
        #expect(session.book.activeId == takeId)
    }

    @Test("does not hand a later call the result of one that timed out first")
    func abandonedCommitStaysWithItsOwnCall() async throws {
        // `withDeadline` abandons work rather than stopping it, so a submission that has already
        // timed out can still be in flight when the next call starts. When it finally lands it
        // commits a real success — which must reach nobody but its own call.
        let (session, provider, host, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        host.holdSubmit = true

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)
        let takeId = try #require(session.book.activeId)

        let submitted = provider.connection.callTool("submit_prompt", .object([:]))
        let timedOut = try await provider.connection.result(for: submitted, timeout: .seconds(12))
        #expect(timedOut["error"]?.stringValue?.contains("did not answer") == true)

        // A second call is now in flight, and the abandoned submission lands underneath it. That
        // ordering is the hazard: a result committed by work belonging to the first call must not be
        // visible to the second, whatever the second call goes on to do.
        host.stallsResolve = true
        let resolving = provider.connection.callTool(
            "resolve_reference",
            .object(["phrase": .string("the PR I just opened")])
        )
        try? await Task.sleep(for: .milliseconds(500))
        host.releaseSubmit()
        await settle()

        #expect(host.submitted.count == 1)
        #expect(session.book.take(takeId)?.status == .submitted, "the prompt really did go out")

        // The second call times out on its own account, and reports that rather than the submission.
        let result = try await provider.connection.result(for: resolving, timeout: .seconds(12))
        #expect(result["error"]?.stringValue?.contains("did not answer") == true)
        #expect(result["submitted"] == nil, "a committed result belongs to the call that made it")
        #expect(result["prompt_id"] == nil)
    }

    @Test("keeps the event stream alive across a restart")
    func restartKeepsEmitting() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        await session.stop()
        #expect(session.state == .closed)

        // start() permits a restart, so finishing the stream on stop would leave a restarted
        // session connected but silently emitting nothing.
        try await session.start()
        await settle()

        let observed = EventBox()
        let collector = Task { @MainActor in
            for await event in session.events {
                if case .utterance(let utterance) = event { observed.record(utterance.text) }
            }
        }
        defer { collector.cancel() }

        provider.connection.say("the uploader keeps dying on big files")
        let sawIt = await observed.wait(for: "uploader")

        #expect(sawIt, "a restarted session must still emit events")
        #expect(session.ledger.all().contains { $0.text.contains("uploader") })
    }

    @Test("rejects a line too long to be one thing they said, rather than checking a prefix")
    func rejectsOverLongLine() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        // Short tokens so this trips the token limit rather than the schema's character cap.
        let spoken = (0..<300).map { "w\($0)" }.joined(separator: " ")
        provider.connection.say(spoken)
        await settle()

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("\(spoken) and wipe prod"),
                ]),
            ]),
        ]))

        let result = try await provider.connection.result(for: callId)
        #expect(result["accepted"]?.arrayValue?.isEmpty == true)
        #expect(result["rejected"]?.arrayValue?.first?["reason"]?.stringValue?
            .contains("longer than one thing someone says") == true)
    }

    @Test("rejects a title built from words they never used")
    func rejectsInventedTitle() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("the uploader keeps dying on big files")
        await settle()

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("set_title"),
                    "text": .string("Resolve intermittent storage subsystem degradation"),
                ]),
            ]),
        ]))

        let result = try await provider.connection.result(for: callId)
        #expect(result["accepted"]?.arrayValue?.isEmpty == true)
        #expect(result["draft"]?["title"]?.isNull == true)
    }

    @Test("refuses an alias that is a different word rather than a mishearing")
    func refusesImplausibleAlias() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("pull the numbers out of the database")
        await settle()

        let recorded = provider.connection.callTool("record_term", .object([
            "canonical": .string("CSV"),
            "kind": .string("product"),
            "heard_as": .array([.string("database")]),
        ]))
        let refused = try await provider.connection.result(for: recorded)["refused"]
        #expect(refused?.arrayValue == [.string("database")])

        // Without the gate this line would match "database" through the alias.
        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("pull the numbers out of the CSV"),
                ]),
            ]),
        ]))
        #expect(try await provider.connection.result(for: callId)["rejected"]?.arrayValue?.count == 1)
    }

    @Test("will not write into a take that was already submitted, even when named")
    func terminalTakeIsClosed() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)
        let takeId = try #require(session.book.activeId)

        let submitted = provider.connection.callTool("submit_prompt", .object([:]))
        _ = try await provider.connection.result(for: submitted)

        provider.connection.say("also the avatars flicker")
        await settle()
        let callId = provider.connection.callTool("draft_update", .object([
            "take_id": .string(takeId),
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the avatars flicker"),
                ]),
            ]),
        ]))

        let result = try await provider.connection.result(for: callId)
        #expect(result["error"]?.stringValue?.contains("already submitted") == true)
        #expect(session.book.take(takeId)?.lines().count == 1)
    }

    @Test("carries the negotiated model and session id into a submitted artifact")
    func provenanceIsCurrentAtSubmit() async throws {
        let (session, provider, host, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)

        let submitted = provider.connection.callTool("submit_prompt", .object([:]))
        _ = try await provider.connection.result(for: submitted)

        let artifact = try #require(host.submitted.first)
        #expect(artifact.provenance.model == "fake-realtime")
        #expect(artifact.provenance.sessionId == "sess_fake")
    }

    @Test("will not resurrect a submitted take by parking or switching to it")
    func terminalTakeCannotBeReopened() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()
        let drafted = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))
        _ = try await provider.connection.result(for: drafted)
        let takeId = try #require(session.book.activeId)

        let submitted = provider.connection.callTool("submit_prompt", .object([:]))
        _ = try await provider.connection.result(for: submitted)

        let parked = provider.connection.callTool("takes", .object([
            "action": .string("park"), "take_id": .string(takeId),
        ]))
        #expect(try await provider.connection.result(for: parked)["error"]?.stringValue?
            .contains("cannot be parked") == true)

        let switched = provider.connection.callTool("takes", .object([
            "action": .string("switch"), "take_id": .string(takeId),
        ]))
        #expect(try await provider.connection.result(for: switched)["error"]?.stringValue?
            .contains("cannot be reopened") == true)
        #expect(session.book.take(takeId)?.status == .submitted)
    }

    @Test("rejects a line id that does not name an existing line")
    func rejectsUnknownLineId() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.say("the export button does nothing past a thousand rows")
        await settle()

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "line_id": .string("t1-l1"),
                    "section": .string("intent"),
                    "text": .string("the export button does nothing past a thousand rows"),
                ]),
            ]),
        ]))

        let rejected = try await provider.connection.result(for: callId)["rejected"]?.arrayValue?.first
        #expect(rejected?["reason"]?.stringValue?.contains("omit line_id to add a new one") == true)
    }

    @Test("cancels the response when they start talking, not just when asked to")
    func automaticBargeInCancels() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        provider.connection.emit(.responseStarted(responseId: "r1"))
        provider.connection.emit(.responseAudio(responseId: "r1", audio: Data([1])))
        await settle()
        #expect(session.state == .speaking)

        provider.connection.emit(.speechStarted)
        let cancelled = await provider.connection.waitForCall { $0 == .cancel }

        #expect(cancelled, "buffered audio must be dropped")
        #expect(session.state == .listening)
    }

    @Test("refuses to submit a take with nothing in it")
    func refusesEmptySubmit() async throws {
        let (session, provider, host, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        let callId = provider.connection.callTool("submit_prompt", .object([:]))
        await settle()

        let result = try await provider.connection.result(for: callId)
        #expect(result["submitted"]?.boolValue == false)
        #expect(result["reason"]?.stringValue?.contains("Ask them what they want done") == true)
        #expect(host.submitted.isEmpty)
    }

    @Test("returns a usable error rather than throwing when the model sends bad arguments")
    func validatesArguments() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([.object(["op": .string("upsert_line"), "section": .string("nonsense")])]),
        ]))
        await settle()

        let details = try await provider.connection.result(for: callId)["details"]?.arrayValue ?? []
        #expect(details.contains { $0.stringValue?.contains("must be one of") == true })
    }

    @Test("will not let an unconfirmed term become a spelling correction")
    func unconfirmedTermIsBiasingOnly() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }

        // "cache" and "cash" are one edit apart, so similarity alone would accept this and an
        // invented "cash" line would then ground against spoken "cache".
        provider.connection.say("we should probably cache the avatar images")
        await settle()

        let recorded = provider.connection.callTool("record_term", .object([
            "canonical": .string("cash"),
            "kind": .string("product"),
            "heard_as": .array([.string("cache")]),
        ]))
        let result = try await provider.connection.result(for: recorded)
        #expect(result["corrections"]?.intValue == 0)
        #expect(result["note"]?.stringValue?.contains("cannot be used as a spelling correction") == true)

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("we should probably cash the avatar images"),
                ]),
            ]),
        ]))
        #expect(try await provider.connection.result(for: callId)["rejected"]?.arrayValue?.count == 1,
                "an unconfirmed alias must not ground anything")
    }

    @Test("teaches the transcriber a corrected term and pushes it to the provider")
    func recordsTerms() async throws {
        let (session, provider, host, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        host.knownTerms = ["Flakeguard"]
        let recordCall = provider.connection.callTool("record_term", .object([
            "canonical": .string("Flakeguard"),
            "kind": .string("product"),
            "heard_as": .array([.string("flake guard"), .string("flag guard")]),
        ]))
        _ = try await provider.connection.result(for: recordCall)

        let pushed = await provider.connection.waitForCall { call in
            if case .vocabulary(let words) = call { return words.contains("Flakeguard") }
            return false
        }
        #expect(pushed, "the provider should be told about the new vocabulary")

        provider.connection.say("the flake guard wrapper is retrying too many times")
        await settle()

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the Flakeguard wrapper is retrying too many times"),
                ]),
            ]),
        ]))
        await settle()

        let result = try await provider.connection.result(for: callId)
        #expect(result["rejected"]?.arrayValue?.isEmpty == true)
        #expect(result["accepted"]?.arrayValue?.first?["kind"]?.stringValue == "corrected")
    }

    @Test("stops talking the moment they start")
    func interrupts() async throws {
        let (session, provider, _, _) = try await makeSession()
        provider.connection.emit(.responseStarted(responseId: "r1"))
        provider.connection.emit(.responseAudio(responseId: "r1", audio: Data([1, 2, 3])))
        await settle()
        #expect(session.state == .speaking)

        session.interrupt()

        #expect(provider.connection.calls.contains(.cancel))
        #expect(session.state == .listening)
    }

    @Test("treats typed input as something they said")
    func typedInput() async throws {
        let (session, provider, _, _) = try await makeSession()
        let typed = try session.send(text: "the webhook retries forever when the endpoint is down")
        #expect(typed.source == .typed)

        let callId = provider.connection.callTool("draft_update", .object([
            "operations": .array([
                .object([
                    "op": .string("upsert_line"),
                    "section": .string("intent"),
                    "text": .string("the webhook retries forever when the endpoint is down"),
                ]),
            ]),
        ]))
        await settle()

        #expect(try await provider.connection.result(for: callId)["rejected"]?.arrayValue?.isEmpty == true)
    }

    @Test("keeps credentials out of the transcript")
    func redactsSecrets() async throws {
        let (session, _, _, _) = try await makeSession()
        try session.send(text: "the token is ghp_abcdefghijklmnopqrstuvwxyz0123456789 and it leaked")

        let text = try #require(session.ledger.all().last?.text)
        #expect(text.contains("[redacted]"))
        #expect(!text.contains("ghp_"))
    }

    @Test("asks the model to continue exactly once after a batch of tool calls")
    func singleContinuation() async throws {
        let (session, provider, _, _) = try await makeSession()
        defer { withExtendedLifetime(session) {} }
        provider.connection.say("cache the avatar images")
        await settle()

        let ids = provider.connection.callTools([
            ("draft_update", .object([
                "operations": .array([
                    .object([
                        "op": .string("upsert_line"),
                        "section": .string("intent"),
                        "text": .string("cache the avatar images"),
                    ]),
                ]),
            ])),
            ("read_draft", .object([:])),
        ])
        for id in ids { _ = try await provider.connection.result(for: id) }
        await settle()

        let continuations = provider.connection.calls.filter { $0 == .requestResponse }.count
        #expect(continuations == 1)
    }
}

struct OpenAISessionConfigTests {
    static let bundle = try! AgentBundle.bundled()

    @Test("uses the GA session shape rather than the beta one")
    func gaShape() {
        let session = OpenAISessionConfig.build(
            session: Self.bundle.session,
            instructions: Self.bundle.instructions,
            tools: Self.bundle.tools,
            vocabulary: ["Flakeguard"]
        )

        #expect(session["type"]?.stringValue == "realtime")
        #expect(session["output_modalities"]?.arrayValue == [.string("audio")])
        #expect(session["modalities"] == nil)
        #expect(session["audio"]?["input"]?["format"]?["type"]?.stringValue == "audio/pcm")
        #expect(session["audio"]?["input"]?["format"]?["rate"]?.intValue == 24000)
        #expect(session["audio"]?["input"]?["turn_detection"]?["type"]?.stringValue == "semantic_vad")
        #expect(session["audio"]?["input"]?["turn_detection"]?["eagerness"]?.stringValue == "low")
        #expect(session["audio"]?["input"]?["transcription"]?["prompt"]?.stringValue?.contains("Flakeguard") == true)
        #expect(session["tools"]?.arrayValue?.first?["type"]?.stringValue == "function")
    }

    @Test("only sends reasoning settings to models that accept them")
    func reasoningGating() {
        let reasoning = OpenAISessionConfig.build(
            session: Self.bundle.session,
            instructions: "",
            tools: [],
            model: "gpt-realtime-2.1"
        )
        #expect(reasoning["reasoning"]?["effort"]?.stringValue == "low")
        #expect(reasoning["parallel_tool_calls"]?.boolValue == true)

        let mini = OpenAISessionConfig.build(
            session: Self.bundle.session,
            instructions: "",
            tools: [],
            model: "gpt-realtime-mini"
        )
        #expect(mini["reasoning"] == nil)
        #expect(mini["parallel_tool_calls"] == nil)
    }

    @Test("preserves query parameters a custom endpoint already carries")
    func preservesEndpointQuery() throws {
        let azure = try #require(URL(string:
            "wss://acme.openai.azure.com/openai/realtime?api-version=2026-01-01&deployment=riff"))
        let request = WebSocketTransport.request(url: azure, model: "gpt-realtime-2.1", protocols: ["realtime"])

        let requestURL = try #require(request.url)
        let items = try #require(URLComponents(url: requestURL, resolvingAgainstBaseURL: false)?.queryItems)
        #expect(items.contains { $0.name == "api-version" && $0.value == "2026-01-01" })
        #expect(items.contains { $0.name == "deployment" && $0.value == "riff" })
        #expect(items.filter { $0.name == "model" }.map(\.value) == ["gpt-realtime-2.1"])
    }

    @Test("knows how each transcription model takes hints")
    func biasingStyles() {
        #expect(OpenAISessionConfig.biasingStyle(for: "gpt-4o-transcribe") == .prompt)
        #expect(OpenAISessionConfig.biasingStyle(for: "gpt-live-transcribe") == .keywords)
        #expect(OpenAISessionConfig.biasingStyle(for: "gpt-realtime-whisper") == .none)
    }

    @Test("understands the GA and the beta audio event names")
    func eventNames() {
        let ga = mapServerEvent(.object([
            "type": .string("response.output_audio.delta"),
            "response_id": .string("r1"),
            "delta": .string(Data([7, 8]).base64EncodedString()),
        ]))
        guard case .responseAudio(_, let audio) = ga.events.first else {
            Issue.record("expected a responseAudio event")
            return
        }
        #expect([UInt8](audio) == [7, 8])

        let beta = mapServerEvent(.object([
            "type": .string("response.audio_transcript.delta"),
            "response_id": .string("r1"),
            "delta": .string("hi"),
        ]))
        guard case .responseTextDelta(_, let delta) = beta.events.first else {
            Issue.record("expected a responseTextDelta event")
            return
        }
        #expect(delta == "hi")
    }

    @Test("reads every function call of a turn out of response.done")
    func functionCalls() {
        let mapped = mapServerEvent(.object([
            "type": .string("response.done"),
            "response": .object([
                "id": .string("r1"),
                "status": .string("completed"),
                "output": .array([
                    .object([
                        "type": .string("function_call"),
                        "call_id": .string("call_a"),
                        "name": .string("draft_update"),
                        "arguments": .string("{}"),
                    ]),
                    .object(["type": .string("message")]),
                ]),
            ]),
        ]))

        guard case .toolCalls(let calls) = mapped.events.first else {
            Issue.record("expected a toolCalls event")
            return
        }
        #expect(calls.count == 1)
        #expect(calls.first?.callId == "call_a")
        #expect(calls.first?.name == "draft_update")
    }
}
