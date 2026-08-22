import Foundation
import RiffCore

/// Where a realtime credential comes from.
///
/// On a device this is always a short-lived client secret minted by your own backend. An API key in
/// an app bundle is a key you have published, so `apiKey` exists for servers and tests only.
public enum RiffCredentials: Sendable {
    case apiKey(String)
    case clientSecret(@Sendable () async throws -> ClientSecret)

    func token() async throws -> String {
        switch self {
        case .apiKey(let key): key
        case .clientSecret(let mint): try await mint().value
        }
    }
}

public struct ClientSecret: Sendable {
    public var value: String
    public var expiresAt: Date?

    public init(value: String, expiresAt: Date? = nil) {
        self.value = value
        self.expiresAt = expiresAt
    }
}

/// Caches a client secret until shortly before it expires, so reconnecting does not make a round
/// trip it does not need.
public actor ClientSecretCache {
    private let mint: @Sendable () async throws -> ClientSecret
    private let margin: TimeInterval
    private var cached: ClientSecret?

    public init(margin: TimeInterval = 30, mint: @escaping @Sendable () async throws -> ClientSecret) {
        self.mint = mint
        self.margin = margin
    }

    public func current() async throws -> ClientSecret {
        if let cached, let expiresAt = cached.expiresAt, expiresAt.timeIntervalSinceNow > margin {
            return cached
        }
        if let cached, cached.expiresAt == nil { return cached }
        let fresh = try await mint()
        cached = fresh
        return fresh
    }
}

public struct MintClientSecretRequest: Sendable {
    /// Server side only. This must never reach a phone.
    public var apiKey: String
    public var session: SessionDefaults
    public var instructions: String
    public var tools: [ToolDefinition]
    public var vocabulary: [String]
    public var model: String?
    /// 10 to 7200 seconds. The API default is 600.
    public var expiresInSeconds: Int?
    public var baseURL: URL
    /// Hashed user id for abuse monitoring. Never send a raw identifier.
    public var safetyIdentifier: String?

    public init(
        apiKey: String,
        session: SessionDefaults,
        instructions: String,
        tools: [ToolDefinition],
        vocabulary: [String] = [],
        model: String? = nil,
        expiresInSeconds: Int? = nil,
        baseURL: URL = URL(string: "https://api.openai.com/v1")!,
        safetyIdentifier: String? = nil
    ) {
        self.apiKey = apiKey
        self.session = session
        self.instructions = instructions
        self.tools = tools
        self.vocabulary = vocabulary
        self.model = model
        self.expiresInSeconds = expiresInSeconds
        self.baseURL = baseURL
        self.safetyIdentifier = safetyIdentifier
    }
}

/// Mints an ephemeral client secret so a device can open a realtime connection without ever holding
/// a real API key. Run this behind your own authenticated endpoint.
public func mintClientSecret(
    _ request: MintClientSecretRequest,
    urlSession: URLSession = .shared
) async throws -> ClientSecret {
    let session = OpenAISessionConfig.build(
        session: request.session,
        instructions: request.instructions,
        tools: request.tools,
        vocabulary: request.vocabulary,
        model: request.model
    )

    var body: [String: JSONValue] = ["session": session]
    if let seconds = request.expiresInSeconds {
        body["expires_after"] = .object([
            "anchor": .string("created_at"),
            "seconds": .number(Double(seconds)),
        ])
    }

    var urlRequest = URLRequest(url: request.baseURL.appendingPathComponent("realtime/client_secrets"))
    urlRequest.httpMethod = "POST"
    urlRequest.setValue("Bearer \(request.apiKey)", forHTTPHeaderField: "Authorization")
    urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
    if let identifier = request.safetyIdentifier {
        urlRequest.setValue(identifier, forHTTPHeaderField: "OpenAI-Safety-Identifier")
    }
    urlRequest.httpBody = JSONValue.object(body).serialized().data(using: .utf8)

    let (data, response) = try await urlSession.data(for: urlRequest)
    guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
        let status = (response as? HTTPURLResponse)?.statusCode ?? -1
        let detail = String(data: data.prefix(300), encoding: .utf8) ?? ""
        throw RiffError.provider("could not mint a realtime client secret (\(status)): \(detail)")
    }

    let decoded = try JSONDecoder().decode(JSONValue.self, from: data)
    guard let value = decoded["value"]?.stringValue else {
        throw RiffError.provider("the client secret response had no value")
    }

    return ClientSecret(
        value: value,
        expiresAt: decoded["expires_at"]?.numberValue.map { Date(timeIntervalSince1970: $0) }
    )
}
