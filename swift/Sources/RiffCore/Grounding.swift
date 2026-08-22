import Foundation

/// A stretch of what the speaker said that a candidate line can be checked against.
public struct SourceSpan: Sendable {
    public var utteranceIds: [String]
    /// Canonicalized, normalized tokens.
    public var tokens: [String]
    /// Utterance id each token came from, parallel to `tokens`.
    public var owners: [String]
    /// Tokens before lexicon canonicalization, for detecting an exact verbatim match.
    public var rawTokens: [String]
}

public struct GroundingResult: Sendable {
    public var ok: Bool
    /// Share of the candidate's meaningful tokens recoverable from a span of what the speaker said.
    public var ratio: Double
    public var kind: GroundingKind
    public var sourceUtteranceIds: [String]
    /// Meaningful candidate tokens with no source. These are the words the agent invented.
    public var unmatchedTokens: [String]
    /// Set when the line failed for a reason the token comparison cannot express.
    public var reason: String?
}

/// Decides whether a line the agent wants to put in the prompt is made of the speaker's words.
///
/// The permitted edit is deletion: drop filler, drop false starts, drop whole sentences, fix a
/// misheard word. That makes a grounded line an ordered subsequence of something the speaker said,
/// modulo lexicon corrections and a small set of connective tokens. Longest common subsequence
/// measures exactly that, and because it is order sensitive it also catches words being shuffled
/// inside a sentence, which reads as a paraphrase even when every word is theirs.
public struct GroundingChecker: Sendable {
    private let config: GroundingConfig
    private let lexicon: Lexicon
    private let free: Set<String>
    private let filler: FillerMatcher

    private static let maxCandidateTokens = 240
    private static let maxSpanTokens = 600

    public init(config: GroundingConfig, lexicon: Lexicon) {
        self.config = config
        self.lexicon = lexicon
        self.free = Set(config.freeTokens.map { $0.lowercased() })
        self.filler = FillerMatcher(config.filler)
    }

    public func check(_ candidate: String, against spans: [SourceSpan]) -> GroundingResult {
        evaluate(candidate, spans, threshold: config.threshold, allowDerived: false)
    }

    /// Looser check for the title, which is a label rather than part of the request.
    public func checkTitle(_ candidate: String, against spans: [SourceSpan]) -> GroundingResult {
        evaluate(candidate, spans, threshold: config.titleThreshold ?? config.threshold, allowDerived: true)
    }

    private func ignorable(_ token: String) -> Bool {
        free.contains(token) || filler.single.contains(token)
    }

    private func evaluate(_ candidate: String, _ spans: [SourceSpan], threshold: Double, allowDerived: Bool) -> GroundingResult {
        let rawTokens = RiffText.tokenize(candidate)

        // Truncating here would check a prefix and let the caller store the whole string, so
        // anything invented past the limit would be recorded as fully grounded.
        if rawTokens.count > Self.maxCandidateTokens {
            return GroundingResult(
                ok: false,
                ratio: 0,
                kind: allowDerived ? .derived : .trimmed,
                sourceUtteranceIds: [],
                unmatchedTokens: [],
                reason: "that line is \(rawTokens.count) words, which is longer than one thing someone says; split it into separate lines"
            )
        }

        let (candTokens, substituted) = lexicon.canonicalize(rawTokens)

        let substantiveIndexes = candTokens.indices.filter { !ignorable(candTokens[$0]) }
        let totalSubstantive = substantiveIndexes.count

        let empty = GroundingResult(
            ok: false,
            ratio: 0,
            kind: allowDerived ? .derived : .trimmed,
            sourceUtteranceIds: [],
            unmatchedTokens: candTokens.filter { !ignorable($0) }
        )

        guard totalSubstantive > 0, !spans.isEmpty else { return empty }

        var candCounts: [String: Int] = [:]
        for index in substantiveIndexes { candCounts[candTokens[index], default: 0] += 1 }

        var best: (ratio: Double, span: SourceSpan, matched: Set<Int>, owners: Set<String>)?

        for span in spans {
            guard !span.tokens.isEmpty else { continue }
            // Multiset containment upper-bounds the ordered match, so this prunes without false negatives.
            guard containment(candCounts, span.tokens, totalSubstantive) >= threshold else { continue }

            let spanTokens = span.tokens.count > Self.maxSpanTokens
                ? Array(span.tokens.suffix(Self.maxSpanTokens))
                : span.tokens
            let offset = span.tokens.count - spanTokens.count

            var matchedIndexes = Set<Int>()
            var matchedOwners = Set<String>()
            var matchedSubstantive = 0

            for pair in longestCommonSubsequence(candTokens, spanTokens) {
                matchedIndexes.insert(pair.0)
                if !ignorable(candTokens[pair.0]) { matchedSubstantive += 1 }
                let ownerIndex = pair.1 + offset
                if ownerIndex < span.owners.count { matchedOwners.insert(span.owners[ownerIndex]) }
            }

            let ratio = Double(matchedSubstantive) / Double(totalSubstantive)
            if best == nil || ratio > best!.ratio {
                best = (ratio, span, matchedIndexes, matchedOwners)
            }
            if ratio == 1 { break }
        }

        guard let best else { return empty }

        let unmatched = candTokens.indices
            .filter { !ignorable(candTokens[$0]) && !best.matched.contains($0) }
            .map { candTokens[$0] }
        let sources = best.span.utteranceIds.filter { best.owners.contains($0) }

        // The lexicon usually corrects the source rather than the candidate: the transcriber wrote
        // "get hub" and the agent wrote "GitHub". Comparing the match with and without
        // canonicalization is what tells a corrected line apart from a merely trimmed one.
        let rawSubstantiveTotal = rawTokens.filter { !ignorable($0) }.count
        var rawMatchedSubstantive = 0
        for pair in longestCommonSubsequence(rawTokens, best.span.rawTokens) where !ignorable(rawTokens[pair.0]) {
            rawMatchedSubstantive += 1
        }
        let rawRatio = rawSubstantiveTotal == 0 ? 0 : Double(rawMatchedSubstantive) / Double(rawSubstantiveTotal)
        let lexiconHelped = substituted || rawRatio < best.ratio

        let kind: GroundingKind
        if best.ratio < threshold {
            kind = allowDerived ? .derived : .trimmed
        } else if lexiconHelped {
            kind = .corrected
        } else if rawTokens == best.span.rawTokens {
            kind = .verbatim
        } else {
            kind = .trimmed
        }

        return GroundingResult(
            ok: best.ratio >= threshold,
            ratio: (best.ratio * 1000).rounded() / 1000,
            kind: kind,
            sourceUtteranceIds: sources,
            unmatchedTokens: unmatched
        )
    }

    /// Fraction of the candidate's meaningful tokens present in the span, counting repeats.
    private func containment(_ candCounts: [String: Int], _ spanTokens: [String], _ total: Int) -> Double {
        guard total > 0 else { return 0 }
        var spanCounts: [String: Int] = [:]
        for token in spanTokens { spanCounts[token, default: 0] += 1 }
        var shared = 0
        for (token, count) in candCounts { shared += min(count, spanCounts[token] ?? 0) }
        return Double(shared) / Double(total)
    }
}

/// Returns matched index pairs of the longest common subsequence of two token sequences.
func longestCommonSubsequence(_ a: [String], _ b: [String]) -> [(Int, Int)] {
    let rows = a.count, cols = b.count
    guard rows > 0, cols > 0 else { return [] }

    let width = cols + 1
    var table = [Int32](repeating: 0, count: (rows + 1) * width)

    for i in stride(from: rows - 1, through: 0, by: -1) {
        for j in stride(from: cols - 1, through: 0, by: -1) {
            table[i * width + j] = a[i] == b[j]
                ? table[(i + 1) * width + (j + 1)] + 1
                : max(table[(i + 1) * width + j], table[i * width + (j + 1)])
        }
    }

    var pairs: [(Int, Int)] = []
    var i = 0, j = 0
    while i < rows, j < cols {
        if a[i] == b[j] {
            pairs.append((i, j))
            i += 1
            j += 1
        } else if table[(i + 1) * width + j] >= table[i * width + (j + 1)] {
            i += 1
        } else {
            j += 1
        }
    }
    return pairs
}
