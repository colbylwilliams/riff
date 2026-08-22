import Foundation

/// A validator for the JSON Schema subset the agent's tool definitions use.
///
/// Tool arguments arrive from a language model, so validation is a hot path on every call and its
/// error messages are read by the model rather than by a person. Both of those argue for a small
/// exact implementation over a general one: the messages name the offending path and say what was
/// expected, and defaults are filled in so a handler never sees a half-populated object.
public enum SchemaValidator {
    public struct Result: Sendable {
        public var valid: Bool
        public var errors: [String]
        public var value: JSONValue
    }

    public static func validate(_ schema: JSONValue, _ input: JSONValue) -> Result {
        var errors: [String] = []
        let value = walk(schema, input, "", &errors)
        return Result(valid: errors.isEmpty, errors: errors, value: value)
    }

    private static func walk(_ schema: JSONValue, _ input: JSONValue, _ path: String, _ errors: inout [String]) -> JSONValue {
        let at = path.isEmpty ? "(root)" : path

        if let constant = schema["const"], constant != input {
            errors.append("\(at): must be \(constant.serialized())")
            return input
        }

        if let options = schema["enum"]?.arrayValue, !options.contains(input) {
            errors.append("\(at): must be one of \(options.map { $0.serialized() }.joined(separator: ", "))")
            return input
        }

        let types = typeNames(schema["type"])
        if !types.isEmpty, !types.contains(where: { matches(type: $0, input) }) {
            errors.append("\(at): expected \(types.joined(separator: " or ")), got \(describe(input))")
            return input
        }

        if let text = input.stringValue {
            // JSON Schema counts code points. `String.count` counts grapheme clusters, which would
            // give a multi-scalar emoji a different length than a conforming validator.
            let length = text.unicodeScalars.count
            if let minimum = schema["minLength"]?.intValue, length < minimum {
                errors.append("\(at): must be at least \(minimum) characters")
            }
            if let maximum = schema["maxLength"]?.intValue, length > maximum {
                errors.append("\(at): must be at most \(maximum) characters")
            }
            if let pattern = schema["pattern"]?.stringValue,
               text.range(of: pattern, options: .regularExpression) == nil {
                errors.append("\(at): must match \(pattern)")
            }
        }

        if let number = input.numberValue {
            if let minimum = schema["minimum"]?.numberValue, number < minimum {
                errors.append("\(at): must be >= \(formatted(minimum))")
            }
            if let maximum = schema["maximum"]?.numberValue, number > maximum {
                errors.append("\(at): must be <= \(formatted(maximum))")
            }
        }

        if let items = input.arrayValue {
            if let minimum = schema["minItems"]?.intValue, items.count < minimum {
                errors.append("\(at): needs at least \(minimum) item\(minimum == 1 ? "" : "s")")
            }
            if let maximum = schema["maxItems"]?.intValue, items.count > maximum {
                errors.append("\(at): allows at most \(maximum) items")
            }
            guard let itemSchema = schema["items"] else { return input }
            return .array(items.enumerated().map { walk(itemSchema, $1, "\(path)[\($0)]", &errors) })
        }

        if let object = input.objectValue {
            let properties = schema["properties"]?.objectValue ?? [:]
            var output: [String: JSONValue] = [:]

            for key in schema["required"]?.arrayValue?.compactMap(\.stringValue) ?? [] where object[key] == nil {
                errors.append("\(at): missing required property \"\(key)\"")
            }

            for (key, raw) in object {
                guard let child = properties[key] else {
                    if schema["additionalProperties"]?.boolValue == false {
                        let known = properties.keys.sorted().joined(separator: ", ")
                        errors.append("\(at): unexpected property \"\(key)\"\(known.isEmpty ? "" : "; allowed: \(known)")")
                    } else {
                        output[key] = raw
                    }
                    continue
                }
                output[key] = walk(child, raw, path.isEmpty ? key : "\(path).\(key)", &errors)
            }

            for (key, child) in properties where output[key] == nil {
                if let fallback = child["default"] { output[key] = fallback }
            }

            return .object(output)
        }

        return input
    }

    private static func typeNames(_ value: JSONValue?) -> [String] {
        guard let value else { return [] }
        if let single = value.stringValue { return [single] }
        return value.arrayValue?.compactMap(\.stringValue) ?? []
    }

    private static func matches(type: String, _ value: JSONValue) -> Bool {
        switch type {
        case "string": value.stringValue != nil
        case "number": value.numberValue != nil
        case "integer": value.intValue != nil && value.numberValue == value.numberValue?.rounded()
        case "boolean": value.boolValue != nil
        case "array": value.arrayValue != nil
        case "object": value.objectValue != nil
        case "null": value.isNull
        default: true
        }
    }

    private static func describe(_ value: JSONValue) -> String {
        switch value {
        case .null: "null"
        case .bool: "boolean"
        case .number: "number"
        case .string: "string"
        case .array: "array"
        case .object: "object"
        }
    }

    private static func formatted(_ value: Double) -> String {
        value == value.rounded() ? String(Int(value)) : String(value)
    }
}
