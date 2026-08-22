import Foundation

/// Text normalization shared by the grounding check, the lexicon, and rendering.
/// Must stay behaviourally identical to the TypeScript implementation; the conformance suite is
/// what proves it.
public enum RiffText {
    /// Splits text into comparable tokens. Digits, dotted identifiers, and hyphenated words stay
    /// whole because `owner/repo#412`, `v1.2.3`, and `cherry-pick` are single things when spoken.
    public static func tokenize(_ text: String) -> [String] {
        var tokens: [String] = []
        var current = ""

        func flush() {
            let normalized = normalizeToken(current)
            if !normalized.isEmpty { tokens.append(normalized) }
            current = ""
        }

        for character in text {
            if character.isLetter || character.isNumber {
                current.append(character)
            } else if !current.isEmpty, character == "'" || character == "\u{2019}" || character == "_"
                || character == "." || character == "-" {
                current.append(character)
            } else {
                flush()
            }
        }
        flush()
        return tokens
    }

    /// Lowercases, folds curly apostrophes, and trims punctuation that survived tokenization.
    public static func normalizeToken(_ raw: String) -> String {
        guard !raw.isEmpty else { return "" }
        var value = raw.lowercased().replacingOccurrences(of: "\u{2019}", with: "'")

        while let first = value.first, first == "." || first == "_" || first == "-" {
            value.removeFirst()
        }
        while let last = value.last, last == "." || last == "_" || last == "-" || last == "'" {
            value.removeLast()
        }
        // A token has to start with a letter or digit, matching the reference tokenizer.
        guard let first = value.first, first.isLetter || first.isNumber else { return "" }
        return value
    }

    /// Collapses whitespace and trims. Used before a line is stored, never to change wording.
    public static func tidyWhitespace(_ text: String) -> String {
        text.split(whereSeparator: { $0.isWhitespace }).joined(separator: " ")
    }

    public static func countWords(_ text: String) -> Int {
        tokenize(text).count
    }
}

/// Single-token filler plus the multi-word filler phrases, split the way the checker needs them.
struct FillerMatcher: Sendable {
    let single: Set<String>
    let phrases: [[String]]

    init(_ filler: [String]) {
        var single = Set<String>()
        var phrases: [[String]] = []
        for entry in filler {
            let tokens = RiffText.tokenize(entry)
            if tokens.count == 1, let token = tokens.first {
                single.insert(token)
            } else if tokens.count > 1 {
                phrases.append(tokens)
            }
        }
        self.single = single
        self.phrases = phrases.sorted { $0.count > $1.count }
    }
}

private let secretPatterns: [NSRegularExpression] = {
    let sources = [
        "\\bgithub_pat_[A-Za-z0-9_]{20,}",
        "\\bgh[pousr]_[A-Za-z0-9]{16,}",
        "\\bsk-[A-Za-z0-9_-]{20,}",
        "\\bek_[A-Za-z0-9_-]{20,}",
        "\\bAKIA[0-9A-Z]{16}\\b",
        "\\bxox[abposr]-[A-Za-z0-9-]{10,}",
        "\\b(?:Bearer|token|api[_-]?key)\\s+[A-Za-z0-9._-]{20,}",
    ]
    return sources.compactMap { try? NSRegularExpression(pattern: $0, options: [.caseInsensitive]) }
}()

/// Strips credentials that can arrive through typed input before anything is stored.
public func redactSecrets(_ text: String) -> String {
    var output = text
    for pattern in secretPatterns {
        output = pattern.stringByReplacingMatches(
            in: output,
            range: NSRange(output.startIndex..., in: output),
            withTemplate: "[redacted]"
        )
    }
    return output
}
