import Foundation

/// Vocabulary the transcriber gets wrong, plus the corrections that fix it.
///
/// A term does two separate jobs, and only one of them is dangerous. Every term biases
/// transcription. Only a *corroborated* term also contributes a grounding alias, because aliases are
/// applied to both sides of a comparison: an alias for a word that is not really the same word lets
/// an invented line match different spoken words, which defeats the fidelity gate. Similarity is not
/// evidence of sameness — "cache" and "cash" are one edit apart and mean nothing alike — so the
/// agent cannot make an alias grounding-active by asserting it.
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
    ///
    /// `corroborated: false` records the term for transcription biasing without letting its aliases
    /// affect grounding. That is the setting for anything the agent asserts on its own.
    public func add(_ term: LexiconTerm, corroborated: Bool = true) {
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

        guard corroborated else { return }

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

private let mishearingSimilarity = 0.6

/// Whether a surface form is plausibly a mishearing of a term, rather than a different word.
///
/// This gates the model-callable `record_term`, and it is what keeps the lexicon from being a hole
/// in the fidelity gate. Aliases are applied to both sides of a grounding comparison, so an alias
/// normally cannot make a paraphrase match — but only because it maps two spellings of the *same*
/// word. An agent that could install `{canonical: "CSV", heardAs: ["database"]}` would make an
/// invented "CSV" line match spoken "database", and the gate would pass it.
///
/// Curated vocabulary is exempt: the seed and workspace lexicons legitimately contain aliases that
/// are not orthographically close, such as "sequel" for SQL or "k8s" for Kubernetes.
public func isPlausibleMishearing(_ heard: String, _ canonical: String) -> Bool {
    func collapse(_ value: String) -> String {
        RiffText.tokenize(value).joined().filter { $0.isLetter || $0.isNumber }
    }

    let a = collapse(heard)
    let b = collapse(canonical)
    guard a.count >= 2, b.count >= 2 else { return false }
    if a == b { return true }

    let distance = editDistance(Array(a), Array(b))
    return 1 - Double(distance) / Double(max(a.count, b.count)) >= mishearingSimilarity
}

private func editDistance(_ a: [Character], _ b: [Character]) -> Int {
    var previous = Array(0...b.count)
    for i in 1...max(a.count, 1) where !a.isEmpty {
        var current = [i]
        for j in 1...max(b.count, 1) where !b.isEmpty {
            current.append(min(
                previous[j] + 1,
                current[j - 1] + 1,
                previous[j - 1] + (a[i - 1] == b[j - 1] ? 0 : 1)
            ))
        }
        previous = current
    }
    return previous[b.count]
}


/// Normalizes a spoken term for comparison as a lexicon key.
public func termKey(_ value: String) -> String {
    RiffText.tokenize(value).joined(separator: " ")
}
