import Foundation

public struct Line: Codable, Sendable {
    public struct Grounding: Codable, Sendable {
        public var ratio: Double
        public var kind: GroundingKind
        public var unmatchedTokens: [String]?
    }

    public var id: String
    public var section: Section
    public var text: String
    public var order: Double
    public var sourceUtteranceIds: [String]
    public var motifId: String?
    public var supersedes: [String]?
    public var grounding: Grounding
}

public struct ContextItem: Codable, Sendable {
    public var referenceId: String
    public var kind: String
    public var title: String
    public var identifier: String?
    public var url: String?
    public var actor: String?
    public var timestamp: String?
    public var state: String?
    public var summary: String?
    /// The phrase the speaker used, so the prompt and the resolved thing stay connected.
    public var resolvedFrom: String?
    public var confidence: Double?

    public init(
        referenceId: String,
        kind: String,
        title: String,
        identifier: String? = nil,
        url: String? = nil,
        actor: String? = nil,
        timestamp: String? = nil,
        state: String? = nil,
        summary: String? = nil,
        resolvedFrom: String? = nil,
        confidence: Double? = nil
    ) {
        self.referenceId = referenceId
        self.kind = kind
        self.title = title
        self.identifier = identifier
        self.url = url
        self.actor = actor
        self.timestamp = timestamp
        self.state = state
        self.summary = summary
        self.resolvedFrom = resolvedFrom
        self.confidence = confidence
    }
}

public struct Motif: Codable, Sendable {
    public var id: String
    public var text: String
    public var scope: String
    public var appliesWhen: String?
    public var createdAt: String
    public var retiredAt: String?

    public init(id: String, text: String, scope: String, appliesWhen: String? = nil, createdAt: String, retiredAt: String? = nil) {
        self.id = id
        self.text = text
        self.scope = scope
        self.appliesWhen = appliesWhen
        self.createdAt = createdAt
        self.retiredAt = retiredAt
    }
}

public enum TakeStatus: String, Codable, Sendable {
    case drafting, ready, submitted, parked, discarded
}

public struct DraftOperation: Sendable {
    public enum Kind: String, Sendable {
        case setTitle = "set_title"
        case upsertLine = "upsert_line"
        case removeLine = "remove_line"
        case moveLine = "move_line"
        case attachContext = "attach_context"
        case detachContext = "detach_context"
    }

    public var op: Kind
    public var lineId: String?
    public var section: String?
    public var text: String?
    /// `.some(nil)` means "place first"; `nil` means "leave where it is" or "append".
    public var afterLineId: String??
    public var supersedes: [String]?
    public var referenceId: String?
}

public struct DraftOperationOutcome: Sendable {
    public var op: String
    public var lineId: String?
    public var accepted: Bool
    /// Written for the model to act on: it says what to do, not just what went wrong.
    public var reason: String?
    public var ratio: Double?
    public var kind: GroundingKind?
    public var unmatchedTokens: [String]?
    public var closestSource: String?
}

private let orderStep: Double = 1000

/// A take that has been sent or thrown away. Nothing may reopen or alter it.
public func isTerminal(_ take: Take) -> Bool {
    take.status == .submitted || take.status == .discarded
}

/// One draft prompt. A session can hold several, so a change of subject does not destroy the last one.
public final class Take: @unchecked Sendable {
    public let id: String
    public var label: String?
    public let createdAt: String
    public var updatedAt: String
    public var status: TakeStatus = .drafting
    public var title: (text: String, origin: String)?
    public var target: String?

    private var linesById: [String: Line] = [:]
    private var contextById: [String: ContextItem] = [:]
    private var contextOrder: [String] = []
    private var pastLines: [Line] = []
    private var sequence = 0

    public init(id: String, createdAt: String, label: String? = nil) {
        self.id = id
        self.createdAt = createdAt
        self.updatedAt = createdAt
        self.label = label
    }

    public func nextLineId() -> String {
        sequence += 1
        return "\(id)-l\(sequence)"
    }

    /// Lines in reading order: section order first, then position within the section.
    public func lines() -> [Line] {
        linesById.values.sorted { lhs, rhs in
            let left = Section.allCases.firstIndex(of: lhs.section) ?? 0
            let right = Section.allCases.firstIndex(of: rhs.section) ?? 0
            if left != right { return left < right }
            if lhs.order != rhs.order { return lhs.order < rhs.order }
            return lhs.id < rhs.id
        }
    }

    public func lines(in section: Section) -> [Line] {
        lines().filter { $0.section == section }
    }

    public func line(_ id: String) -> Line? { linesById[id] }

    /// In the order the speaker brought each reference up, matching the reference implementation.
    /// Sorting by id instead would reorder the rendered context block, since ids come from the host.
    public func context() -> [ContextItem] {
        contextOrder.compactMap { contextById[$0] }
    }

    /// Lines replaced by a correction. Kept so a change of mind can be walked back.
    public func history() -> [Line] { pastLines }

    public func setLine(_ line: Line) {
        if let existing = linesById[line.id] { pastLines.append(existing) }
        linesById[line.id] = line
    }

    @discardableResult
    public func removeLine(_ id: String) -> Bool {
        guard let existing = linesById[id] else { return false }
        pastLines.append(existing)
        linesById[id] = nil
        return true
    }

    public func attach(_ item: ContextItem) {
        if contextById[item.referenceId] == nil { contextOrder.append(item.referenceId) }
        contextById[item.referenceId] = item
    }

    @discardableResult
    public func detach(_ referenceId: String) -> Bool {
        guard contextById.removeValue(forKey: referenceId) != nil else { return false }
        contextOrder.removeAll { $0 == referenceId }
        return true
    }

    public var isEmpty: Bool { linesById.isEmpty && contextById.isEmpty }

    /// Order value that places a new line immediately after `afterId` within its section.
    public func order(in section: Section, after afterId: String??) -> Double {
        let siblings = lines(in: section)
        switch afterId {
        case .none:
            return (siblings.last?.order ?? 0) + orderStep
        case .some(.none):
            return (siblings.first?.order ?? orderStep * 2) - orderStep
        case .some(.some(let anchorId)):
            guard let index = siblings.firstIndex(where: { $0.id == anchorId }) else {
                return (siblings.last?.order ?? 0) + orderStep
            }
            let anchor = siblings[index]
            let next = index + 1 < siblings.count ? siblings[index + 1] : nil
            return next.map { (anchor.order + $0.order) / 2 } ?? anchor.order + orderStep
        }
    }

    /// Whether the take has everything the policy says a sendable prompt needs.
    public func isReady(policy: PolicyConfig) -> Bool {
        policy.readinessRequires.allSatisfy { !lines(in: $0).isEmpty }
    }
}

public struct ApplyContext: Sendable {
    public var checker: GroundingChecker
    public var spans: [SourceSpan]
    public var grounding: GroundingConfig
    /// Resolved references the agent may attach, keyed by reference id.
    public var references: [String: ContextItem]
    public var motifs: [String: Motif]
    public var now: String
}

/// Applies the agent's draft operations, refusing any line that is not made of the speaker's words.
///
/// Rejection is the mechanism that keeps the prompt in their voice: the model proposes text, this
/// function decides whether it survives, and the rejection message tells the model exactly which
/// words it invented so it can put theirs back.
public func applyDraftOperations(
    to take: Take,
    operations: [DraftOperation],
    context: ApplyContext
) -> (accepted: [DraftOperationOutcome], rejected: [DraftOperationOutcome]) {
    var accepted: [DraftOperationOutcome] = []
    var rejected: [DraftOperationOutcome] = []

    func record(_ outcome: DraftOperationOutcome) {
        if outcome.accepted { accepted.append(outcome) } else { rejected.append(outcome) }
    }

    for operation in operations {
        switch operation.op {
        case .setTitle:
            record(applySetTitle(take, operation, context))

        case .upsertLine:
            record(applyUpsert(take, operation, context))

        case .removeLine:
            if let id = operation.lineId, take.removeLine(id) {
                record(DraftOperationOutcome(op: operation.op.rawValue, lineId: id, accepted: true))
            } else {
                record(DraftOperationOutcome(
                    op: operation.op.rawValue,
                    lineId: operation.lineId,
                    accepted: false,
                    reason: "no line \(operation.lineId ?? "(missing line_id)") in this take"
                ))
            }

        case .moveLine:
            if let id = operation.lineId, var line = take.line(id) {
                line.order = take.order(in: line.section, after: operation.afterLineId)
                take.setLine(line)
                record(DraftOperationOutcome(op: operation.op.rawValue, lineId: id, accepted: true))
            } else {
                record(DraftOperationOutcome(
                    op: operation.op.rawValue,
                    lineId: operation.lineId,
                    accepted: false,
                    reason: "no line \(operation.lineId ?? "(missing line_id)") in this take"
                ))
            }

        case .attachContext:
            if let id = operation.referenceId, let reference = context.references[id] {
                take.attach(reference)
                record(DraftOperationOutcome(op: operation.op.rawValue, accepted: true))
            } else {
                record(DraftOperationOutcome(
                    op: operation.op.rawValue,
                    accepted: false,
                    reason: "unknown reference_id \(operation.referenceId ?? "(missing)"); call resolve_reference first and use an id it returned"
                ))
            }

        case .detachContext:
            let detached = operation.referenceId.map { take.detach($0) } ?? false
            record(DraftOperationOutcome(
                op: operation.op.rawValue,
                accepted: detached,
                reason: detached ? nil : "that reference is not attached"
            ))
        }
    }

    if !accepted.isEmpty { take.updatedAt = context.now }
    return (accepted, rejected)
}

private func applySetTitle(_ take: Take, _ operation: DraftOperation, _ context: ApplyContext) -> DraftOperationOutcome {
    let text = RiffText.tidyWhitespace(operation.text ?? "")
    guard !text.isEmpty else {
        return DraftOperationOutcome(op: "set_title", accepted: false, reason: "set_title needs text")
    }
    let result = context.checker.checkTitle(text, against: context.spans)
    guard result.ok else {
        // A title is held to a looser bar than a body line, but it may still only use words they
        // used. Accepting it as `derived` here would make the looser bar no bar at all.
        return DraftOperationOutcome(
            op: "set_title",
            accepted: false,
            reason: result.reason ?? rejectionReason(result.unmatchedTokens, context.spans),
            ratio: result.ratio,
            unmatchedTokens: result.unmatchedTokens
        )
    }

    take.title = (text, "spoken")
    return DraftOperationOutcome(op: "set_title", accepted: true, ratio: result.ratio, kind: result.kind)
}

private func applyUpsert(_ take: Take, _ operation: DraftOperation, _ context: ApplyContext) -> DraftOperationOutcome {
    let text = RiffText.tidyWhitespace(operation.text ?? "")
    guard let raw = operation.section, let section = Section(rawValue: raw) else {
        return DraftOperationOutcome(
            op: "upsert_line",
            accepted: false,
            reason: "section must be one of \(Section.allCases.map(\.rawValue).joined(separator: ", "))"
        )
    }
    guard !text.isEmpty else {
        return DraftOperationOutcome(op: "upsert_line", accepted: false, reason: "upsert_line needs text")
    }

    let mode = context.grounding.sections[section.rawValue] ?? .strict
    let motif = context.motifs.values.first {
        $0.retiredAt == nil && $0.text.lowercased() == text.lowercased()
    }

    var grounding: Line.Grounding
    var sourceUtteranceIds: [String] = []
    var motifId: String?

    if mode == .motifOrStrict, let motif {
        grounding = Line.Grounding(ratio: 1, kind: .motif)
        motifId = motif.id
    } else {
        let result = context.checker.check(text, against: context.spans)
        guard result.ok else {
            return DraftOperationOutcome(
                op: "upsert_line",
                lineId: operation.lineId,
                accepted: false,
                reason: result.reason ?? rejectionReason(result.unmatchedTokens, context.spans),
                ratio: result.ratio,
                unmatchedTokens: result.unmatchedTokens,
                closestSource: closestSource(context.spans, result.sourceUtteranceIds)
            )
        }
        grounding = Line.Grounding(
            ratio: result.ratio,
            kind: result.kind,
            unmatchedTokens: result.unmatchedTokens.isEmpty ? nil : result.unmatchedTokens
        )
        sourceUtteranceIds = result.sourceUtteranceIds
    }

    let existing = operation.lineId.flatMap { take.line($0) }
    if let supplied = operation.lineId, existing == nil {
        // Accepting an unknown id would create a line outside the generated sequence, and the next
        // ordinary insert would reuse that id and silently overwrite this line.
        return DraftOperationOutcome(
            op: "upsert_line",
            lineId: supplied,
            accepted: false,
            reason: "no line \(supplied) in this take; omit line_id to add a new one"
        )
    }
    let id = existing?.id ?? take.nextLineId()
    let order: Double
    if let existing, operation.afterLineId == nil {
        order = existing.order
    } else {
        order = take.order(in: section, after: operation.afterLineId)
    }

    var supersedes: [String] = []
    for target in operation.supersedes ?? [] where target != id && !supersedes.contains(target) {
        supersedes.append(target)
        take.removeLine(target)
    }

    take.setLine(Line(
        id: id,
        section: section,
        text: text,
        order: order,
        sourceUtteranceIds: sourceUtteranceIds,
        motifId: motifId,
        supersedes: supersedes.isEmpty ? nil : supersedes,
        grounding: grounding
    ))

    return DraftOperationOutcome(
        op: "upsert_line",
        lineId: id,
        accepted: true,
        ratio: grounding.ratio,
        kind: grounding.kind
    )
}

private func rejectionReason(_ unmatched: [String], _ spans: [SourceSpan]) -> String {
    if spans.isEmpty { return "nothing has been said yet, so there is nothing to draw on" }
    if unmatched.isEmpty { return "the words are theirs but the order is not; keep their phrasing intact" }
    let shown = unmatched.prefix(6).map { "\"\($0)\"" }.joined(separator: ", ")
    let more = unmatched.count > 6 ? " and \(unmatched.count - 6) more" : ""
    return "they did not say \(shown)\(more); use their words or ask them"
}

private func closestSource(_ spans: [SourceSpan], _ utteranceIds: [String]) -> String? {
    guard !utteranceIds.isEmpty else { return nil }
    let wanted = Set(utteranceIds)
    return spans.first { !$0.utteranceIds.filter(wanted.contains).isEmpty }?.tokens.joined(separator: " ")
}

/// Holds every take in the session and tracks which one is being spoken into.
public final class DraftBook: @unchecked Sendable {
    private var takesById: [String: Take] = [:]
    private var order: [String] = []
    private var sequence = 0
    private let policy: PolicyConfig
    private let clock: @Sendable () -> String

    public private(set) var activeId: String?

    public init(policy: PolicyConfig, now: @escaping @Sendable () -> String = { ISO8601.now() }) {
        self.policy = policy
        self.clock = now
    }

    public func takes() -> [Take] { order.compactMap { takesById[$0] } }

    public func take(_ id: String) -> Take? { takesById[id] }

    /// The take being spoken into, creating the first one on demand.
    public func active() throws -> Take {
        if let activeId, let take = takesById[activeId] { return take }
        return try create()
    }

    @discardableResult
    public func create(label: String? = nil, carryContextFrom source: Take? = nil) throws -> Take {
        let live = takes().filter { $0.status != .discarded && $0.status != .submitted }
        if live.count >= policy.maxTakes {
            if let oldest = live.min(by: { $0.updatedAt < $1.updatedAt }), oldest.isEmpty {
                takesById[oldest.id] = nil
                order.removeAll { $0 == oldest.id }
            } else {
                throw RiffError.tool("too many open takes (limit \(policy.maxTakes)); park or send one first")
            }
        }

        sequence += 1
        let take = Take(id: "t\(sequence)", createdAt: clock(), label: label)
        for item in source?.context() ?? [] { take.attach(item) }
        takesById[take.id] = take
        order.append(take.id)
        activeId = take.id
        return take
    }

    @discardableResult
    public func switchTo(_ id: String) throws -> Take? {
        guard let take = takesById[id] else { return nil }
        // Switching to a finished take would make it the target of the next thing spoken.
        guard !isTerminal(take) else {
            throw RiffError.tool("take \"\(id)\" was already \(take.status.rawValue); it cannot be reopened")
        }
        if let activeId, activeId != id, let previous = takesById[activeId], previous.status == .drafting {
            previous.status = .parked
        }
        if take.status == .parked { take.status = .drafting }
        activeId = id
        return take
    }

    @discardableResult
    public func park(_ id: String) throws -> Take? {
        guard let take = takesById[id] else { return nil }
        // Parking a finished take would move it out of a terminal state, and switching back would
        // then promote it to drafting — which is how every guard downstream gets bypassed.
        guard !isTerminal(take) else {
            throw RiffError.tool("take \"\(id)\" was already \(take.status.rawValue); it cannot be parked")
        }
        take.status = .parked
        if activeId == id {
            activeId = takes().first { $0.id != id && $0.status == .drafting }?.id
        }
        return take
    }

    @discardableResult
    public func discard(_ id: String) throws -> Bool {
        guard let take = takesById[id] else { return false }
        guard take.status != .submitted else {
            throw RiffError.tool("take \"\(id)\" was already submitted")
        }
        take.status = .discarded
        if activeId == id { activeId = nil }
        return true
    }

    public func clearActive() { activeId = nil }
}
