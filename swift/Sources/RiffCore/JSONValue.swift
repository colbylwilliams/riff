import Foundation

/// A JSON value of unknown shape. Tool arguments arrive from a language model, so they cannot be
/// decoded into a fixed type until they have been validated.
public enum JSONValue: Sendable, Hashable {
    case null
    case bool(Bool)
    case number(Double)
    case string(String)
    case array([JSONValue])
    case object([String: JSONValue])

    public var stringValue: String? { if case .string(let value) = self { return value } else { return nil } }
    public var boolValue: Bool? { if case .bool(let value) = self { return value } else { return nil } }
    public var arrayValue: [JSONValue]? { if case .array(let value) = self { return value } else { return nil } }
    public var objectValue: [String: JSONValue]? { if case .object(let value) = self { return value } else { return nil } }

    public var numberValue: Double? {
        if case .number(let value) = self { return value }
        return nil
    }

    public var intValue: Int? {
        guard let number = numberValue else { return nil }
        return Int(exactly: number.rounded())
    }

    public subscript(key: String) -> JSONValue? { objectValue?[key] }

    public var isNull: Bool { if case .null = self { return true } else { return false } }
}

extension JSONValue: Codable {
    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else {
            throw DecodingError.dataCorruptedError(in: container, debugDescription: "unsupported JSON value")
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value):
            // Whole numbers encode without a trailing ".0" so payloads match the reference output.
            if let integer = Int(exactly: value.rounded()), value == value.rounded() {
                try container.encode(integer)
            } else {
                try container.encode(value)
            }
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }
}

public extension JSONValue {
    /// Parses a JSON string, as tool arguments arrive.
    static func parse(_ text: String) throws -> JSONValue {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return .object([:]) }
        guard let data = trimmed.data(using: .utf8) else {
            throw RiffError.invalidArguments(["arguments were not valid UTF-8"])
        }
        return try JSONDecoder().decode(JSONValue.self, from: data)
    }

    func serialized() -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes]
        guard let data = try? encoder.encode(self), let text = String(data: data, encoding: .utf8) else {
            return "{}"
        }
        return text
    }
}

public enum RiffError: Error, CustomStringConvertible, Sendable {
    case bundleInvalid(String)
    case invalidArguments([String])
    case unknownTool(String)
    case notConnected
    case provider(String)
    case tool(String)

    public var description: String {
        switch self {
        case .bundleInvalid(let reason): "the agent bundle is not usable: \(reason)"
        case .invalidArguments(let reasons): "invalid arguments: \(reasons.joined(separator: "; "))"
        case .unknownTool(let name): "unknown tool \"\(name)\""
        case .notConnected: "the session is not connected"
        case .provider(let message): message
        case .tool(let message): message
        }
    }
}
