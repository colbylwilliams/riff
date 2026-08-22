import Foundation

/// Vocabulary the transcriber gets wrong, plus the corrections that fix it.
///
/// The lexicon is applied to both sides of every grounding comparison, so an alias can never make a
/// paraphrase look grounded — it only lets "get hub" and "GitHub" recognize each other.
public final class Lexicon: @unchecked Sendable {
    private var termsByKey: [String: LexiconTerm] = [:]
    private var aliases: [String: (tokens: [String], term: LexiconTerm)] = [:]
    private var maxAliasLength = 1
    private let lock = NSLock()

    public init(_ terms: [LexiconTerm] = []) {
        for term in terms { add(term) }
    }

    public var count: Int {
        lock.lock(); defer { lock.unlock() }
        return termsByKey.count
    }

    public func terms() -> [LexiconTerm] {
        lock.lock(); defer { lock.unlock() }
        return Array(termsByKey.values)
    }

    /// Adds or replaces a term. Aliases identical to the canonical form are ignored as no-ops.
    public func add(_ term: LexiconTerm) {
        let canonicalTokens = RiffText.tokenize(term.canonical)
        guard !canonicalTokens.isEmpty else { return }
        let key = canonicalTokens.joined(separator: " ")

        lock.lock(); defer { lock.unlock() }

        var merged = term
        if let existing = termsByKey[key] {
            var combined = existing.heardAs ?? []
            for alias in term.heardAs ?? [] where !combined.contains(alias) { combined.append(alias) }
            merged = term
            merged.heardAs = combined.isEmpty ? nil : combined
            if merged.definition == nil { merged.definition = existing.definition }
        }
        termsByKey[key] = merged

        for alias in merged.heardAs ?? [] {
            let aliasTokens = RiffText.tokenize(alias)
            guard !aliasTokens.isEmpty else { continue }
            let aliasKey = aliasTokens.joined(separator: " ")
            if aliasKey == key { continue }
            aliases[aliasKey] = (canonicalTokens, merged)
            maxAliasLength = max(maxAliasLength, aliasTokens.count)
        }
    }

    /// Looks up what the transcriber may have meant by a surface form.
    public func lookup(_ heard: String) -> [LexiconTerm] {
        let key = RiffText.tokenize(heard).joined(separator: " ")
        guard !key.isEmpty else { return [] }
        lock.lock(); defer { lock.unlock() }
        if let direct = termsByKey[key] { return [direct] }
        if let alias = aliases[key] { return [alias.term] }
        return []
    }

    /// Rewrites alias phrases to canonical form, longest match first.
    public func canonicalize(_ tokens: [String]) -> (tokens: [String], substituted: Bool) {
        lock.lock()
        let aliases = self.aliases
        let maxAliasLength = self.maxAliasLength
        lock.unlock()

        guard !aliases.isEmpty else { return (tokens, false) }

        var output: [String] = []
        output.reserveCapacity(tokens.count)
        var substituted = false
        var index = 0

        while index < tokens.count {
            var matched = false
            let upperBound = min(maxAliasLength, tokens.count - index)
            var length = upperBound
            while length >= 1 {
                let key = tokens[index..<(index + length)].joined(separator: " ")
                if let alias = aliases[key] {
                    output.append(contentsOf: alias.tokens)
                    substituted = true
                    index += length
                    matched = true
                    break
                }
                length -= 1
            }
            if !matched {
                output.append(tokens[index])
                index += 1
            }
        }

        return (output, substituted)
    }

    /// Canonical spellings for transcription biasing, most mangle-prone first.
    ///
    /// Ordering is part of the contract rather than a detail: this list is capped before it reaches
    /// the provider, so the comparator decides which terms survive, and different biasing produces
    /// different transcripts. Both scoring and tie-breaking are defined without locale rules so
    /// every binding produces the same list.
    public func keywords(limit: Int = 100) -> [String] {
        terms()
            .sorted { lhs, rhs in
                let left = biasingScore(lhs), right = biasingScore(rhs)
                if left != right { return left > right }
                return compareByCodePoint(lhs.canonical, rhs.canonical) < 0
            }
            .prefix(limit)
            .map(\.canonical)
    }

    /// Terms whose canonical or alias forms appear in the given text, for artifact provenance.
    public func termsUsed(in text: String) -> [LexiconTerm] {
        let tokens = RiffText.tokenize(text)
        lock.lock()
        let termsByKey = self.termsByKey
        let aliases = self.aliases
        let maxAliasLength = self.maxAliasLength
        lock.unlock()

        var used: [String: LexiconTerm] = [:]
        var order: [String] = []
        var index = 0
        while index < tokens.count {
            var length = min(maxAliasLength, tokens.count - index)
            while length >= 1 {
                let key = tokens[index..<(index + length)].joined(separator: " ")
                if let term = termsByKey[key] ?? aliases[key]?.term {
                    if used[term.canonical] == nil {
                        used[term.canonical] = term
                        order.append(term.canonical)
                    }
                    break
                }
                length -= 1
            }
            index += 1
        }
        return order.compactMap { used[$0] }
    }
}

/// How strongly a term should be biased toward during transcription. Terms with recorded
/// mishearings, acronym shapes, and multi-word names are the ones recognizers actually get wrong;
/// workspace vocabulary outranks general vocabulary because it is what this speaker will say.
public func biasingScore(_ term: LexiconTerm) -> Int {
    var value = (term.heardAs?.count ?? 0) * 2
    // Two consecutive capitals, so acronyms score but ordinary CamelCase product names do not.
    let characters = Array(term.canonical)
    if characters.indices.dropLast().contains(where: { characters[$0].isUppercase && characters[$0 + 1].isUppercase }) {
        value += 3
    }
    if term.canonical.contains(where: { $0 == " " || $0 == "-" || $0 == "_" || $0 == "/" }) { value += 2 }
    if term.scope == "workspace" { value += 4 }
    if term.scope == "user" { value += 2 }
    return value
}

/// Case-insensitive comparison by code point, falling back to the raw form.
///
/// Deliberately not the default string ordering: this has to match the reference implementation
/// exactly, because it decides which terms survive the biasing cap.
public func compareByCodePoint(_ a: String, _ b: String) -> Int {
    func compare(_ left: String, _ right: String) -> Int {
        let leftPoints = Array(left.unicodeScalars)
        let rightPoints = Array(right.unicodeScalars)
        for index in 0..<min(leftPoints.count, rightPoints.count) where leftPoints[index] != rightPoints[index] {
            return leftPoints[index].value < rightPoints[index].value ? -1 : 1
        }
        return leftPoints.count - rightPoints.count
    }
    let folded = compare(a.lowercased(), b.lowercased())
    return folded != 0 ? folded : compare(a, b)
}
