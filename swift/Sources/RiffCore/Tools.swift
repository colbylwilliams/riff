import Foundation

public struct ToolOutcome: Sendable {
    public var ok: Bool
    public var result: JSONValue
    public var durationMs: Int
}

/// Builds a JSON object, dropping keys whose value is absent.
func json(_ pairs: [(String, JSONValue?)]) -> JSONValue {
    var object: [String: JSONValue] = [:]
    for (key, value) in pairs {
        if let value { object[key] = value }
    }
    return .object(object)
}

func jsonString(_ value: String?) -> JSONValue? { value.map { .string($0) } }
func jsonNumber(_ value: Double?) -> JSONValue? { value.map { .number($0) } }
func jsonInt(_ value: Int?) -> JSONValue? { value.map { .number(Double($0)) } }

/// Everything a tool call needs to read and change. Confined to the main actor because the draft is
/// what a user interface renders, and there is exactly one conversation in flight at a time.
@MainActor
public final class ToolRuntime {
    public let bundle: AgentBundle
    public let ledger: UtteranceLedger
    public let book: DraftBook
    public let lexicon: Lexicon
    public let checker: GroundingChecker
    public let host: any RiffHost
    public let store: any RiffStore

    /// References resolved this session, so the agent can attach one by id later.
    public var references: [String: ContextItem] = [:]
    public var motifs: [String: Motif] = [:]
    public var provenance: PromptArtifact.Provenance?

    var onLexiconChanged: (() -> Void)?
    var onDraftChanged: ((Take) -> Void)?
    var onTakeChanged: ((Take?) -> Void)?
    var onSubmitted: ((PromptArtifact) -> Void)?

    public init(
        bundle: AgentBundle,
        ledger: UtteranceLedger,
        book: DraftBook,
        lexicon: Lexicon,
        checker: GroundingChecker,
        host: any RiffHost,
        store: any RiffStore
    ) {
        self.bundle = bundle
        self.ledger = ledger
        self.book = book
        self.lexicon = lexicon
        self.checker = checker
        self.host = host
        self.store = store
    }

    func now() -> String { ISO8601.now() }
}

@MainActor
public final class ToolRegistry {
    private let runtime: ToolRuntime
    private let definitions: [String: ToolDefinition]

    public init(runtime: ToolRuntime) {
        self.runtime = runtime
        self.definitions = Dictionary(uniqueKeysWithValues: runtime.bundle.tools.map { ($0.name, $0) })
    }

    public func dispatch(name: String, argumentsJson: String) async -> ToolOutcome {
        let startedAt = Date()
        func finish(_ ok: Bool, _ result: JSONValue) -> ToolOutcome {
            ToolOutcome(ok: ok, result: result, durationMs: Int(Date().timeIntervalSince(startedAt) * 1000))
        }

        guard let definition = definitions[name] else {
            return finish(false, json([("error", .string("unknown tool \"\(name)\""))]))
        }

        let parsed: JSONValue
        do {
            parsed = try JSONValue.parse(argumentsJson)
        } catch {
            return finish(false, json([("error", .string("arguments were not valid JSON"))]))
        }

        let validated = SchemaValidator.validate(definition.parameters, parsed)
        guard validated.valid else {
            return finish(false, json([
                ("error", .string("invalid arguments")),
                ("details", .array(validated.errors.map { .string($0) })),
            ]))
        }

        do {
            return finish(true, try await run(name: name, args: validated.value))
        } catch {
            return finish(false, json([("error", .string(String(describing: error)))]))
        }
    }

    // MARK: - Handlers

    private func run(name: String, args: JSONValue) async throws -> JSONValue {
        switch name {
        case "draft_update": return try draftUpdate(args)
        case "read_draft": return try readDraft(args)
        case "resolve_reference": return try await resolveReference(args)
        case "lookup_term": return try await lookupTerm(args)
        case "record_term": return try await recordTerm(args)
        case "recall_prompts": return try await recallPrompts(args)
        case "motifs": return try await motifs(args)
        case "takes": return try takes(args)
        case "submit_prompt": return try await submitPrompt(args)
        default: throw RiffError.unknownTool(name)
        }
    }

    private func take(from args: JSONValue) throws -> Take {
        if let id = args["take_id"]?.stringValue {
            guard let take = runtime.book.take(id) else { throw RiffError.tool("no take \"\(id)\"") }
            return take
        }
        return try runtime.book.active()
    }

    /// Compact view of the draft returned after every mutation, so the agent always knows line ids.
    private func draftView(_ take: Take, includeRendered: Bool = false) throws -> JSONValue {
        var grouped: [String: [JSONValue]] = [:]
        for line in take.lines() {
            grouped[line.section.rawValue, default: []].append(json([
                ("id", .string(line.id)),
                ("text", .string(line.text)),
                ("grounding", .string(line.grounding.kind.rawValue)),
            ]))
        }

        return json([
            ("take_id", .string(take.id)),
            ("label", jsonString(take.label)),
            ("title", take.title.map { JSONValue.string($0.text) } ?? .null),
            ("sections", .object(grouped.mapValues { JSONValue.array($0) })),
            ("context", .array(take.context().map { item in
                json([
                    ("reference_id", .string(item.referenceId)),
                    ("identifier", jsonString(item.identifier ?? item.title)),
                    ("url", jsonString(item.url)),
                ])
            })),
            ("ready", .bool(take.isReady(policy: runtime.bundle.policy))),
            ("rendered", includeRendered
                ? .string(try renderPrompt(take, options: RenderOptions(config: runtime.bundle.render)))
                : nil),
        ])
    }

    private func fidelity(of take: Take) throws -> Double {
        try buildArtifact(take, options: BuildArtifactOptions(
            render: RenderOptions(config: runtime.bundle.render),
            lexicon: runtime.lexicon,
            utteranceCount: runtime.ledger.count,
            now: runtime.now()
        )).provenance.fidelity
    }

    private func outcomeJSON(_ outcome: DraftOperationOutcome) -> JSONValue {
        json([
            ("op", .string(outcome.op)),
            ("line_id", jsonString(outcome.lineId)),
            ("reason", jsonString(outcome.reason)),
            ("ratio", jsonNumber(outcome.ratio)),
            ("kind", jsonString(outcome.kind?.rawValue)),
            ("unmatched_tokens", outcome.unmatchedTokens.map { .array($0.map { .string($0) }) }),
            ("closest_source", jsonString(outcome.closestSource)),
        ])
    }

    private func draftUpdate(_ args: JSONValue) throws -> JSONValue {
        let take = try take(from: args)
        let operations = (args["operations"]?.arrayValue ?? []).compactMap(parseOperation)

        let (accepted, rejected) = applyDraftOperations(
            to: take,
            operations: operations,
            context: ApplyContext(
                checker: runtime.checker,
                spans: runtime.ledger.spans(),
                grounding: runtime.bundle.grounding,
                references: runtime.references,
                motifs: runtime.motifs,
                now: runtime.now()
            )
        )

        if !accepted.isEmpty { runtime.onDraftChanged?(take) }

        return json([
            ("accepted", .array(accepted.map(outcomeJSON))),
            ("rejected", .array(rejected.map(outcomeJSON))),
            ("fidelity", .number(try fidelity(of: take))),
            ("draft", try draftView(take)),
        ])
    }

    private func parseOperation(_ value: JSONValue) -> DraftOperation? {
        guard let raw = value["op"]?.stringValue, let op = DraftOperation.Kind(rawValue: raw) else { return nil }
        // A present `after_line_id` of null means "place first", while an absent one means "append".
        let after: String?? = value["after_line_id"].map { $0.isNull ? nil : $0.stringValue }
        return DraftOperation(
            op: op,
            lineId: value["line_id"]?.stringValue,
            section: value["section"]?.stringValue,
            text: value["text"]?.stringValue,
            afterLineId: after,
            supersedes: value["supersedes"]?.arrayValue?.compactMap(\.stringValue),
            referenceId: value["reference_id"]?.stringValue
        )
    }

    private func readDraft(_ args: JSONValue) throws -> JSONValue {
        let take = try take(from: args)
        var view = try draftView(take, includeRendered: args["include_rendered"]?.boolValue == true).objectValue ?? [:]
        view["gist"] = .string(summarizeDraft(take))
        view["fidelity"] = .number(try fidelity(of: take))
        return .object(view)
    }

    private func resolveReference(_ args: JSONValue) async throws -> JSONValue {
        let phrase = args["phrase"]?.stringValue ?? ""
        let kind = args["kind"]?.stringValue
        let candidates = try await runtime.host.resolveReference(ResolveReferenceRequest(
            phrase: phrase,
            kind: kind == "unknown" ? nil : kind,
            recency: args["recency"]?.stringValue,
            actor: args["actor"]?.stringValue,
            limit: args["limit"]?.intValue,
            transcript: runtime.ledger.recentText()
        ))

        var described: [JSONValue] = []
        for (index, candidate) in candidates.enumerated() {
            let referenceId = candidate.referenceId.isEmpty
                ? "r\(runtime.references.count + index + 1)"
                : candidate.referenceId
            var item = candidate
            item.referenceId = referenceId
            item.resolvedFrom = phrase
            runtime.references[referenceId] = item

            described.append(json([
                ("reference_id", .string(referenceId)),
                ("kind", .string(item.kind)),
                ("title", .string(item.title)),
                ("identifier", jsonString(item.identifier)),
                ("url", jsonString(item.url)),
                ("actor", jsonString(item.actor)),
                ("timestamp", jsonString(item.timestamp)),
                ("state", jsonString(item.state)),
                ("confidence", jsonNumber(item.confidence)),
            ]))
        }

        return json([
            ("candidates", .array(described)),
            ("note", described.isEmpty
                ? .string("nothing matched; ask them which one they mean rather than guessing")
                : nil),
        ])
    }

    private func lookupTerm(_ args: JSONValue) async throws -> JSONValue {
        let heard = args["heard"]?.stringValue ?? ""
        let kind = args["kind"]?.stringValue

        func described(_ term: LexiconTerm, confidence: Double?, source: String) -> JSONValue {
            json([
                ("canonical", .string(term.canonical)),
                ("kind", .string(term.kind)),
                ("definition", jsonString(term.definition)),
                ("heard_as", term.heardAs.map { .array($0.map { .string($0) }) }),
                ("scope", jsonString(term.scope)),
                ("confidence", jsonNumber(confidence)),
                ("source", .string(source)),
            ])
        }

        var matches = runtime.lexicon.lookup(heard).map { described($0, confidence: 1, source: "lexicon") }
        let seen = Set(runtime.lexicon.lookup(heard).map { $0.canonical.lowercased() })

        let remote = try await runtime.host.lookupTerm(LookupTermRequest(
            heard: heard,
            context: args["context"]?.stringValue,
            kind: kind == "unknown" ? nil : kind
        ))

        // The host's confidence has to survive: a fuzzy search guess and a confirmed glossary hit
        // are the difference between correcting a word silently and asking about it.
        for match in remote where !seen.contains(match.term.canonical.lowercased()) {
            matches.append(described(match.term, confidence: match.confidence, source: "host"))
        }

        return json([
            ("matches", .array(matches)),
            ("note", matches.isEmpty ? .string("unknown here; if it matters, ask them what it is") : nil),
        ])
    }

    private func recordTerm(_ args: JSONValue) async throws -> JSONValue {
        let scope = args["scope"]?.stringValue ?? "user"
        let heardAs = args["heard_as"]?.arrayValue?.compactMap(\.stringValue)
        let term = LexiconTerm(
            canonical: RiffText.tidyWhitespace(args["canonical"]?.stringValue ?? ""),
            kind: args["kind"]?.stringValue ?? "other",
            heardAs: (heardAs?.isEmpty ?? true) ? nil : heardAs,
            definition: args["definition"]?.stringValue,
            scope: scope
        )
        guard !term.canonical.isEmpty else { throw RiffError.tool("record_term needs a canonical spelling") }

        runtime.lexicon.add(term)
        runtime.ledger.invalidate()
        if scope != "session" { try await runtime.store.saveTerm(term) }
        runtime.onLexiconChanged?()

        return json([
            ("recorded", .string(term.canonical)),
            ("corrections", .number(Double(term.heardAs?.count ?? 0))),
        ])
    }

    private func recallPrompts(_ args: JSONValue) async throws -> JSONValue {
        let prompts = try await runtime.host.recallPrompts(RecallPromptsRequest(
            query: args["query"]?.stringValue ?? "",
            recency: args["recency"]?.stringValue,
            status: args["status"]?.stringValue,
            limit: args["limit"]?.intValue
        ))

        return json([("prompts", .array(prompts.map { prompt in
            json([
                ("prompt_id", .string(prompt.promptId)),
                ("title", .string(prompt.title)),
                ("excerpt", .string(prompt.excerpt)),
                ("submitted_at", jsonString(prompt.submittedAt)),
                ("status", jsonString(prompt.status)),
                ("outcome", jsonString(prompt.outcome)),
                ("url", jsonString(prompt.url)),
            ])
        }))])
    }

    private func motifs(_ args: JSONValue) async throws -> JSONValue {
        let action = args["action"]?.stringValue ?? ""

        switch action {
        case "list":
            let listed = runtime.motifs.values
                .filter { $0.retiredAt == nil }
                .sorted { $0.id < $1.id }
                .map { motif in
                    json([
                        ("motif_id", .string(motif.id)),
                        ("text", .string(motif.text)),
                        ("scope", .string(motif.scope)),
                        ("applies_when", jsonString(motif.appliesWhen)),
                    ])
                }
            return json([("motifs", .array(listed))])

        case "save":
            let text = RiffText.tidyWhitespace(args["text"]?.stringValue ?? "")
            guard !text.isEmpty else { throw RiffError.tool("save needs the text of the standing instruction") }

            let grounding = runtime.checker.check(text, against: runtime.ledger.spans())
            guard grounding.ok else {
                let words = grounding.unmatchedTokens.prefix(6).map { "\"\($0)\"" }.joined(separator: ", ")
                return json([
                    ("saved", .bool(false)),
                    ("reason", .string("a motif has to be their wording; they did not say \(words)")),
                ])
            }

            let motif = Motif(
                id: "m\(runtime.motifs.count + 1)",
                text: text,
                scope: args["scope"]?.stringValue ?? "user",
                appliesWhen: args["applies_when"]?.stringValue,
                createdAt: runtime.now()
            )
            runtime.motifs[motif.id] = motif
            try await runtime.store.saveMotif(motif)
            return json([("saved", .bool(true)), ("motif_id", .string(motif.id))])

        case "attach":
            guard let id = args["motif_id"]?.stringValue, let motif = runtime.motifs[id] else {
                throw RiffError.tool("no motif \"\(args["motif_id"]?.stringValue ?? "(missing motif_id)")\"")
            }
            let take = try runtime.book.active()
            if take.lines().contains(where: { $0.motifId == motif.id }) {
                return json([("attached", .bool(false)), ("reason", .string("already on this take"))])
            }

            let lineId = take.nextLineId()
            take.setLine(Line(
                id: lineId,
                section: .constraint,
                text: motif.text,
                order: take.order(in: .constraint, after: nil),
                sourceUtteranceIds: [],
                motifId: motif.id,
                supersedes: nil,
                grounding: Line.Grounding(ratio: 1, kind: .motif)
            ))
            take.updatedAt = runtime.now()
            runtime.onDraftChanged?(take)
            return json([
                ("attached", .bool(true)),
                ("line_id", .string(lineId)),
                ("draft", try draftView(take)),
            ])

        case "detach":
            let take = try runtime.book.active()
            guard let line = take.lines().first(where: { $0.motifId == args["motif_id"]?.stringValue }) else {
                return json([("detached", .bool(false)), ("reason", .string("not on this take"))])
            }
            take.removeLine(line.id)
            take.updatedAt = runtime.now()
            runtime.onDraftChanged?(take)
            return json([("detached", .bool(true))])

        case "retire":
            guard let id = args["motif_id"]?.stringValue, var motif = runtime.motifs[id] else {
                throw RiffError.tool("retire needs a known motif_id")
            }
            let at = runtime.now()
            motif.retiredAt = at
            runtime.motifs[id] = motif
            try await runtime.store.retireMotif(id: id, at: at)
            return json([("retired", .bool(true))])

        default:
            throw RiffError.tool("unknown motifs action \"\(action)\"")
        }
    }

    private func takes(_ args: JSONValue) throws -> JSONValue {
        let action = args["action"]?.stringValue ?? ""
        let book = runtime.book

        switch action {
        case "new":
            let carry = args["carry_context"]?.boolValue == true
            let previous = book.activeId.flatMap { book.take($0) }
            let take = try book.create(label: args["label"]?.stringValue, carryContextFrom: carry ? previous : nil)
            runtime.onTakeChanged?(take)
            return json([
                ("take_id", .string(take.id)),
                ("label", jsonString(take.label)),
                ("active", .bool(true)),
            ])

        case "switch":
            guard let id = args["take_id"]?.stringValue, let take = book.switchTo(id) else {
                throw RiffError.tool("no take \"\(args["take_id"]?.stringValue ?? "(missing take_id)")\"")
            }
            runtime.onTakeChanged?(take)
            return json([("take_id", .string(take.id)), ("draft", try draftView(take))])

        case "park":
            guard let id = args["take_id"]?.stringValue ?? book.activeId, let take = book.park(id) else {
                throw RiffError.tool("there is no take to park")
            }
            runtime.onTakeChanged?(book.activeId.flatMap { book.take($0) })
            return json([("parked", .string(take.id))])

        case "list":
            return json([("takes", .array(book.takes().map { take in
                json([
                    ("take_id", .string(take.id)),
                    ("label", jsonString(take.label)),
                    ("status", .string(take.status.rawValue)),
                    ("active", .bool(take.id == book.activeId)),
                    ("title", take.title.map { JSONValue.string($0.text) } ?? .null),
                    ("lines", .number(Double(take.lines().count))),
                    ("updated_at", .string(take.updatedAt)),
                ])
            }))])

        case "discard":
            guard let id = args["take_id"]?.stringValue ?? book.activeId, book.discard(id) else {
                throw RiffError.tool("there is no take to discard")
            }
            runtime.onTakeChanged?(book.activeId.flatMap { book.take($0) })
            return json([("discarded", .string(id))])

        default:
            throw RiffError.tool("unknown takes action \"\(action)\"")
        }
    }

    private func submitPrompt(_ args: JSONValue) async throws -> JSONValue {
        let take = try take(from: args)
        let policy = runtime.bundle.policy

        guard take.isReady(policy: policy) else {
            let missing = policy.readinessRequires
                .filter { take.lines(in: $0).isEmpty }
                .map(\.rawValue)
                .joined(separator: " or ")
            return json([
                ("submitted", .bool(false)),
                ("reason", .string("nothing to send yet: no \(missing) captured. Ask them what they want done.")),
            ])
        }

        let target = args["target"]?.stringValue
        let keepOpen = args["keep_open"]?.boolValue == true
        if let target { take.target = target }
        take.status = .ready

        let artifact = try buildArtifact(take, options: BuildArtifactOptions(
            render: RenderOptions(config: runtime.bundle.render),
            lexicon: runtime.lexicon,
            utteranceCount: runtime.ledger.count,
            now: runtime.now(),
            provenance: runtime.provenance
        ))

        let result = try await runtime.host.submitPrompt(
            artifact,
            options: SubmitOptions(target: target, keepOpen: keepOpen)
        )

        if result.submitted {
            take.status = keepOpen ? .drafting : .submitted
            var stored = artifact
            stored.status = take.status
            try await runtime.store.saveArtifact(stored)
            runtime.onSubmitted?(stored)
            if !keepOpen {
                runtime.book.clearActive()
                runtime.onTakeChanged?(nil)
            }
        } else {
            take.status = .drafting
        }

        return json([
            ("submitted", .bool(result.submitted)),
            ("prompt_id", .string(result.promptId ?? artifact.id)),
            ("destination", jsonString(result.destination)),
            ("url", jsonString(result.url)),
            ("message", jsonString(result.message)),
        ])
    }
}
