import Foundation

public struct PromptArtifact: Codable, Sendable {
    public struct Title: Codable, Sendable {
        public var text: String
        public var origin: String
    }

    public struct TermUse: Codable, Sendable {
        public var canonical: String
        public var kind: String?
        public var heardAs: [String]?
        public var definition: String?
    }

    public struct ToolCall: Codable, Sendable {
        public var name: String
        public var at: String
        public var durationMs: Int?
        public var ok: Bool?
    }

    public struct Provenance: Codable, Sendable {
        /// Token-weighted share of the prompt body recoverable from what the speaker said.
        public var fidelity: Double
        public var utteranceCount: Int
        public var bodyTokens: Int
        public var agentAuthoredTokens: Int
        public var agentVersion: String?
        public var bundleRevision: String?
        public var providerId: String?
        public var model: String?
        public var sessionId: String?
        public var durationMs: Int?
        public var toolCalls: [ToolCall]?
    }

    public var id: String
    public var takeId: String
    public var label: String?
    public var createdAt: String
    public var updatedAt: String
    public var title: Title
    public var lines: [Line]
    public var context: [ContextItem]
    public var terms: [TermUse]?
    public var provenance: Provenance
    /// The prompt as the downstream agent receives it.
    public var rendered: String
    public var target: String?
    public var status: TakeStatus?
}

public struct RenderOptions: Sendable {
    public var config: RenderConfig
    /// Overrides `config.profile` when a host wants a different shape for a specific destination.
    public var profile: String?

    public init(config: RenderConfig, profile: String? = nil) {
        self.config = config
        self.profile = profile
    }
}

/// Renders a take as the Markdown the downstream agent receives.
public func renderPrompt(_ take: Take, options: RenderOptions) throws -> String {
    let name = options.profile ?? options.config.profile
    guard let profile = options.config.profiles[name] else {
        throw RiffError.bundleInvalid("unknown render profile \"\(name)\"")
    }

    var blocks: [String] = []
    if let title = take.title, profile["title"] == .h1 {
        blocks.append("# \(title.text)")
    }

    for section in Section.allCases {
        let lines = take.lines(in: section)
        guard !lines.isEmpty else { continue }
        let disposition = profile[section.rawValue] ?? .paragraphs
        if let block = renderSection(section, lines.map(\.text), disposition, options.config.labels) {
            blocks.append(block)
        }
    }

    let context = take.context()
    if !context.isEmpty {
        let disposition = profile["context"] ?? .labeledList
        if let block = renderContext(context, disposition, options.config.labels) {
            blocks.append(block)
        }
    }

    return blocks.joined(separator: "\n\n").trimmingCharacters(in: .whitespacesAndNewlines)
}

private func renderSection(_ section: Section, _ texts: [String], _ disposition: RenderDisposition, _ labels: [String: String]) -> String? {
    let label = labels[section.rawValue] ?? titleCase(section.rawValue)
    switch disposition {
    case .omit:
        return nil
    case .paragraphs:
        return texts.map(withTerminalPunctuation).joined(separator: " ")
    case .labeledList:
        return (["**\(label)**"] + texts.map { "- \($0)" }).joined(separator: "\n")
    case .sectionList:
        return (["## \(label)", ""] + texts.map { "- \($0)" }).joined(separator: "\n")
    case .h1:
        return "# \(texts.joined(separator: " "))"
    }
}

private func renderContext(_ items: [ContextItem], _ disposition: RenderDisposition, _ labels: [String: String]) -> String? {
    guard disposition != .omit else { return nil }
    let label = labels["context"] ?? "Context"
    let entries = items.map { "- \(describe($0))" }
    return disposition == .sectionList
        ? (["## \(label)", ""] + entries).joined(separator: "\n")
        : (["**\(label)**"] + entries).joined(separator: "\n")
}

private func describe(_ item: ContextItem) -> String {
    var parts: [String] = [item.identifier ?? (item.title.isEmpty ? item.kind : item.title)]
    if item.identifier != nil, !item.title.isEmpty { parts.append("\"\(item.title)\"") }
    if let state = item.state { parts.append("(\(state))") }
    if let actor = item.actor { parts.append("by \(actor)") }
    if let url = item.url { parts.append("— \(url)") }
    if let resolvedFrom = item.resolvedFrom { parts.append("— referred to as \"\(resolvedFrom)\"") }
    return parts.joined(separator: " ")
}

private func withTerminalPunctuation(_ text: String) -> String {
    let trimmed = RiffText.tidyWhitespace(text)
    guard let last = trimmed.last else { return trimmed }
    return ".!?:;".contains(last) ? trimmed : trimmed + "."
}

private func titleCase(_ section: String) -> String {
    let spaced = section.replacingOccurrences(of: "_", with: " ")
    return spaced.prefix(1).uppercased() + spaced.dropFirst()
}

public struct BuildArtifactOptions: Sendable {
    public var render: RenderOptions
    public var lexicon: Lexicon
    public var utteranceCount: Int
    public var now: String
    public var provenance: PromptArtifact.Provenance?

    public init(
        render: RenderOptions,
        lexicon: Lexicon,
        utteranceCount: Int,
        now: String = ISO8601.now(),
        provenance: PromptArtifact.Provenance? = nil
    ) {
        self.render = render
        self.lexicon = lexicon
        self.utteranceCount = utteranceCount
        self.now = now
        self.provenance = provenance
    }
}

/// Turns a take into the artifact that leaves the session.
///
/// Fidelity is a token-weighted share of the body that is provably the speaker's, so a consumer can
/// tell at a glance whether a prompt was captured or composed, without reading it.
public func buildArtifact(_ take: Take, options: BuildArtifactOptions) throws -> PromptArtifact {
    let lines = take.lines()
    let rendered = try renderPrompt(take, options: options.render)

    var bodyTokens = 0
    var groundedTokens = 0.0
    for line in lines {
        let words = RiffText.countWords(line.text)
        bodyTokens += words
        groundedTokens += Double(words) * (line.grounding.kind == .derived ? 0 : line.grounding.ratio)
    }

    let fidelity = bodyTokens == 0 ? 0 : ((groundedTokens / Double(bodyTokens)) * 1000).rounded() / 1000
    let terms = options.lexicon.termsUsed(in: lines.map(\.text).joined(separator: " "))

    var provenance = options.provenance ?? PromptArtifact.Provenance(
        fidelity: 0, utteranceCount: 0, bodyTokens: 0, agentAuthoredTokens: 0
    )
    provenance.fidelity = fidelity
    provenance.utteranceCount = options.utteranceCount
    provenance.bodyTokens = bodyTokens
    provenance.agentAuthoredTokens = Int((Double(bodyTokens) - groundedTokens).rounded())

    let title = take.title.map { PromptArtifact.Title(text: $0.text, origin: $0.origin) }
        ?? PromptArtifact.Title(text: fallbackTitle(take), origin: "derived")

    return PromptArtifact(
        id: "\(take.id)-\(options.now)",
        takeId: take.id,
        label: take.label,
        createdAt: take.createdAt,
        updatedAt: take.updatedAt,
        title: title,
        lines: lines,
        context: take.context(),
        terms: terms.isEmpty ? nil : terms.map {
            PromptArtifact.TermUse(canonical: $0.canonical, kind: $0.kind, heardAs: $0.heardAs, definition: $0.definition)
        },
        provenance: provenance,
        rendered: rendered,
        target: take.target,
        status: take.status
    )
}

private func fallbackTitle(_ take: Take) -> String {
    guard let first = take.lines(in: .intent).first ?? take.lines().first else { return "Untitled" }
    let words = first.text.split(separator: " ").prefix(8).joined(separator: " ")
    return words.hasSuffix(",") || words.hasSuffix(".") || words.hasSuffix(";") || words.hasSuffix(":")
        ? String(words.dropLast())
        : words
}

/// A short account of what the draft covers, for when they ask how it is looking.
/// This is the agent's own summary and never becomes part of the prompt.
public func summarizeDraft(_ take: Take) -> String {
    let counts = Section.allCases
        .map { ($0, take.lines(in: $0).count) }
        .filter { $0.1 > 0 }
    guard !counts.isEmpty else { return "Nothing captured yet." }

    let described = counts
        .map { "\($0.1) \($0.0.rawValue.replacingOccurrences(of: "_", with: " "))\($0.1 == 1 ? "" : "s")" }
        .joined(separator: ", ")
    let context = take.context()
    let attached = context.isEmpty
        ? ""
        : " Attached: \(context.map { $0.identifier ?? $0.title }.joined(separator: ", "))."
    let intent = take.lines(in: .intent).first.map { "\($0.text) " } ?? ""

    return "\(intent)Captured \(described).\(attached)".trimmingCharacters(in: .whitespaces)
}
