import Foundation

public struct ResolveReferenceRequest: Sendable {
    public var phrase: String
    public var kind: String?
    public var recency: String?
    public var actor: String?
    public var limit: Int?
    /// Recent transcript, so a host can disambiguate from what was being discussed.
    public var transcript: String?
}

public struct LookupTermRequest: Sendable {
    public var heard: String
    public var context: String?
    public var kind: String?
}

public struct RecallPromptsRequest: Sendable {
    public var query: String
    public var recency: String?
    public var status: String?
    public var limit: Int?
}

public struct PriorPrompt: Codable, Sendable {
    public var promptId: String
    public var title: String
    public var excerpt: String
    public var submittedAt: String?
    public var status: String?
    public var outcome: String?
    public var url: String?

    public init(promptId: String, title: String, excerpt: String, submittedAt: String? = nil, status: String? = nil, outcome: String? = nil, url: String? = nil) {
        self.promptId = promptId
        self.title = title
        self.excerpt = excerpt
        self.submittedAt = submittedAt
        self.status = status
        self.outcome = outcome
        self.url = url
    }
}

public struct SubmitOptions: Sendable {
    public var target: String?
    public var keepOpen: Bool
}

public struct SubmitResult: Sendable {
    public var submitted: Bool
    public var promptId: String?
    public var destination: String?
    public var url: String?
    public var message: String?

    public init(submitted: Bool, promptId: String? = nil, destination: String? = nil, url: String? = nil, message: String? = nil) {
        self.submitted = submitted
        self.promptId = promptId
        self.destination = destination
        self.url = url
        self.message = message
    }
}

public struct HostEnvironment: Sendable {
    public struct Destination: Sendable {
        public var id: String
        public var label: String
        public var isDefault: Bool

        public init(id: String, label: String, isDefault: Bool = false) {
            self.id = id
            self.label = label
            self.isDefault = isDefault
        }
    }

    /// Where the speaker is working, phrased as they would say it.
    public var workspace: String?
    public var repository: String?
    public var branch: String?
    public var userLogin: String?
    public var userName: String?
    /// Destinations `submit_prompt` may target.
    public var destinations: [Destination]
    /// Names, repos, and jargon specific to this workspace, folded into the lexicon at connect time.
    public var vocabulary: [LexiconTerm]
    /// Things recently touched, which make "the one I just opened" resolvable.
    public var recent: [ContextItem]

    public init(
        workspace: String? = nil,
        repository: String? = nil,
        branch: String? = nil,
        userLogin: String? = nil,
        userName: String? = nil,
        destinations: [Destination] = [],
        vocabulary: [LexiconTerm] = [],
        recent: [ContextItem] = []
    ) {
        self.workspace = workspace
        self.repository = repository
        self.branch = branch
        self.userLogin = userLogin
        self.userName = userName
        self.destinations = destinations
        self.vocabulary = vocabulary
        self.recent = recent
    }
}

/// What the embedding application supplies.
///
/// Riff knows how to keep a prompt in someone's voice; it does not know what their world contains.
/// Everything world shaped comes through this protocol, which is why the same agent works in an
/// editor, a terminal, a phone, and a design tool without knowing the difference.
public protocol RiffHost: Sendable {
    /// Turns "the PR I just opened" into a thing with an identifier and a URL.
    func resolveReference(_ request: ResolveReferenceRequest) async throws -> [ContextItem]
    /// Says what a term means here and how it is spelled.
    func lookupTerm(_ request: LookupTermRequest) async throws -> [LexiconTerm]
    /// Finds prompts the speaker wrote before.
    func recallPrompts(_ request: RecallPromptsRequest) async throws -> [PriorPrompt]
    /// Hands the finished prompt to whatever does the work.
    func submitPrompt(_ artifact: PromptArtifact, options: SubmitOptions) async throws -> SubmitResult
    /// Ambient facts that make references resolvable without asking.
    func environment() async throws -> HostEnvironment
}

public extension RiffHost {
    func environment() async throws -> HostEnvironment { HostEnvironment() }
}

/// A host for when there is nothing to look things up in.
///
/// It resolves nothing rather than guessing, because a fabricated PR number sends the downstream
/// agent somewhere real and wrong, which is worse than an unresolved reference the agent asks about.
public struct NullHost: RiffHost {
    public init() {}
    public func resolveReference(_ request: ResolveReferenceRequest) async throws -> [ContextItem] { [] }
    public func lookupTerm(_ request: LookupTermRequest) async throws -> [LexiconTerm] { [] }
    public func recallPrompts(_ request: RecallPromptsRequest) async throws -> [PriorPrompt] { [] }
    public func submitPrompt(_ artifact: PromptArtifact, options: SubmitOptions) async throws -> SubmitResult {
        SubmitResult(submitted: true, promptId: artifact.id, destination: "none")
    }
}

/// Persistence for the things that outlive a session.
public protocol RiffStore: Sendable {
    func loadLexicon() async throws -> [LexiconTerm]
    func saveTerm(_ term: LexiconTerm) async throws
    func listMotifs() async throws -> [Motif]
    func saveMotif(_ motif: Motif) async throws
    func retireMotif(id: String, at: String) async throws
    func saveArtifact(_ artifact: PromptArtifact) async throws
    func listArtifacts(limit: Int) async throws -> [PromptArtifact]
}

public actor MemoryStore: RiffStore {
    private var terms: [LexiconTerm]
    private var motifs: [String: Motif] = [:]
    private var artifacts: [PromptArtifact] = []

    public init(terms: [LexiconTerm] = [], motifs: [Motif] = []) {
        self.terms = terms
        for motif in motifs { self.motifs[motif.id] = motif }
    }

    public func loadLexicon() async throws -> [LexiconTerm] { terms }

    public func saveTerm(_ term: LexiconTerm) async throws {
        if let index = terms.firstIndex(where: { $0.canonical.lowercased() == term.canonical.lowercased() }) {
            terms[index] = term
        } else {
            terms.append(term)
        }
    }

    public func listMotifs() async throws -> [Motif] {
        motifs.values.filter { $0.retiredAt == nil }.sorted { $0.id < $1.id }
    }

    public func saveMotif(_ motif: Motif) async throws { motifs[motif.id] = motif }

    public func retireMotif(id: String, at: String) async throws {
        motifs[id]?.retiredAt = at
    }

    public func saveArtifact(_ artifact: PromptArtifact) async throws { artifacts.append(artifact) }

    public func listArtifacts(limit: Int) async throws -> [PromptArtifact] {
        Array(artifacts.suffix(limit).reversed())
    }
}
