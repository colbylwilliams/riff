import Foundation

/// Sections of a prompt, in the order they matter to the downstream agent.
public enum Section: String, Codable, Sendable, CaseIterable {
    case intent
    case detail
    case constraint
    case acceptance
    case openQuestion = "open_question"
}

/// How a line in the prompt relates to what the speaker actually said.
public enum GroundingKind: String, Codable, Sendable {
    case verbatim
    case trimmed
    case corrected
    case motif
    case derived
}

/// How strictly a section is held to the speaker's words.
public enum GroundingMode: String, Codable, Sendable {
    case strict
    case motifOrStrict = "motif-or-strict"
    case derived
}

public struct LexiconTerm: Codable, Sendable, Hashable {
    public var canonical: String
    public var kind: String
    public var heardAs: [String]?
    public var definition: String?
    public var scope: String?

    public init(canonical: String, kind: String, heardAs: [String]? = nil, definition: String? = nil, scope: String? = nil) {
        self.canonical = canonical
        self.kind = kind
        self.heardAs = heardAs
        self.definition = definition
        self.scope = scope
    }
}

public struct ToolDefinition: Codable, Sendable {
    public enum Kind: String, Codable, Sendable { case local, host }
    public var name: String
    public var kind: Kind
    public var description: String
    public var parameters: JSONValue
    public var returns: JSONValue?
}

public struct InstructionSection: Codable, Sendable {
    public var id: String
    public var title: String
    public var order: Int
    public var source: String
    public var text: String
}

public struct GroundingConfig: Codable, Sendable {
    public var threshold: Double
    public var titleThreshold: Double?
    public var windowSize: Int
    public var sections: [String: GroundingMode]
    public var freeTokens: [String]
    public var filler: [String]
}

public enum RenderDisposition: String, Codable, Sendable {
    case h1
    case paragraphs
    case labeledList = "labeled-list"
    case sectionList = "section-list"
    case omit
}

public struct RenderConfig: Codable, Sendable {
    public var profile: String
    public var profiles: [String: [String: RenderDisposition]]
    public var labels: [String: String]
    public var includeProvenanceFooter: Bool?
}

public struct PolicyConfig: Codable, Sendable {
    public var maxTakes: Int
    public var readinessRequires: [Section]
    public var autoSubmit: Bool
    public var readbackDefault: String
    public var persistAudio: Bool?
    public var redactSecretsFromTranscript: Bool?
}

/// Provider-neutral session defaults. Providers map these onto their own wire format.
public struct SessionDefaults: Codable, Sendable {
    public struct Model: Codable, Sendable {
        public var preferred: String
        public var fallbacks: [String]?
    }

    public struct Modalities: Codable, Sendable {
        public var input: [String]
        public var output: [String]
    }

    public struct Voice: Codable, Sendable {
        public var name: String
        public var speed: Double?
    }

    public struct AudioStream: Codable, Sendable {
        public var encoding: String
        public var sampleRate: Int
        public var channels: Int
        public var noiseReduction: String?
    }

    public struct Audio: Codable, Sendable {
        public var input: AudioStream
        public var output: AudioStream
    }

    public struct ServerVAD: Codable, Sendable {
        public var threshold: Double
        public var prefixPaddingMs: Int
        public var silenceDurationMs: Int
    }

    public struct TurnDetection: Codable, Sendable {
        public enum Mode: String, Codable, Sendable { case semantic, vad, manual }
        public var mode: Mode
        public var eagerness: String?
        public var autoRespond: Bool
        public var allowBargeIn: Bool
        public var serverVadFallback: ServerVAD?
    }

    public struct Biasing: Codable, Sendable {
        public var enabled: Bool
        public var maxKeywords: Int
        public var promptPreamble: String
    }

    public struct Transcription: Codable, Sendable {
        public var preferred: String
        public var fallbacks: [String]?
        public var language: String?
        public var biasing: Biasing?
    }

    public struct Reasoning: Codable, Sendable {
        public var effort: String
    }

    public struct Limits: Codable, Sendable {
        public var maxOutputTokens: Int
        public var maxSessionSeconds: Int
        public var toolTimeoutMs: Int
    }

    public struct Truncation: Codable, Sendable {
        public var strategy: String
        public var retentionRatio: Double
        public var postInstructionTokenLimit: Int
    }

    public var model: Model
    public var modalities: Modalities
    public var voice: Voice
    public var audio: Audio
    public var turnDetection: TurnDetection
    public var transcription: Transcription
    public var reasoning: Reasoning?
    public var limits: Limits
    public var truncation: Truncation?
    public var toolChoice: String
    public var parallelToolCalls: Bool?
}

public struct SeedLexicon: Codable, Sendable {
    public var version: Int
    public var terms: [LexiconTerm]
}

/// The shared agent definition, compiled from `core/agent` and shipped to every platform binding.
public struct AgentBundle: Codable, Sendable {
    public var id: String
    public var name: String
    public var version: String
    public var revision: String
    public var description: String?
    /// The composed system prompt handed to the realtime model.
    public var instructions: String
    public var instructionSections: [InstructionSection]
    public var tools: [ToolDefinition]
    public var session: SessionDefaults
    public var lexicon: SeedLexicon
    public var grounding: GroundingConfig
    public var render: RenderConfig
    public var policy: PolicyConfig

    /// Loads the bundle shipped inside the package.
    public static func bundled() throws -> AgentBundle {
        guard let url = Bundle.module.url(forResource: "riff-agent.bundle", withExtension: "json") else {
            throw RiffError.bundleInvalid("riff-agent.bundle.json is missing from the package resources")
        }
        return try load(from: try Data(contentsOf: url))
    }

    /// Loads and validates a bundle, refusing anything that would let the agent drift from its contract.
    public static func load(from data: Data) throws -> AgentBundle {
        let bundle: AgentBundle
        do {
            bundle = try JSONDecoder().decode(AgentBundle.self, from: data)
        } catch {
            throw RiffError.bundleInvalid(String(describing: error))
        }

        guard !bundle.instructions.isEmpty else { throw RiffError.bundleInvalid("instructions are empty") }
        guard !bundle.tools.isEmpty else { throw RiffError.bundleInvalid("there are no tools") }
        guard bundle.grounding.threshold > 0, bundle.grounding.threshold <= 1 else {
            throw RiffError.bundleInvalid("grounding.threshold must be greater than 0 and at most 1")
        }
        guard bundle.grounding.windowSize >= 1 else {
            throw RiffError.bundleInvalid("grounding.windowSize must be at least 1")
        }
        guard bundle.render.profiles[bundle.render.profile] != nil else {
            throw RiffError.bundleInvalid("render profile \"\(bundle.render.profile)\" is not defined")
        }
        guard bundle.policy.autoSubmit == false else {
            throw RiffError.bundleInvalid("policy.autoSubmit must be false: the speaker decides when a prompt is sent")
        }
        return bundle
    }
}
