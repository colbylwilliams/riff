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

            var matchedIndexes = Set<Int>()
            var matchedOwners = Set<String>()
            var matchedSubstantive = 0

            for pair in longestCommonSubsequence(candTokens, span.tokens) {
                matchedIndexes.insert(pair.0)
                if !ignorable(candTokens[pair.0]) { matchedSubstantive += 1 }
                if pair.1 < span.owners.count { matchedOwners.insert(span.owners[pair.1]) }
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

/// Matched index pairs of the longest common subsequence, in O(min) memory.
///
/// A full DP table would be quadratic, which is why earlier versions capped the source span and
/// aligned only a window of it. Every one of those caps was wrong in a way nobody could see: a cap
/// on the candidate let invented text through, and a cap on the source rejected lines that were
/// entirely the speaker's. Hirschberg's divide and conquer gives the same alignment while holding
/// only two rows at a time, so the whole span is always compared and there is no cap to be wrong
/// about.
public func longestCommonSubsequence(_ a: [String], _ b: [String]) -> [(Int, Int)] {
    var pairs: [(Int, Int)] = []
    align(a, 0, a.count, b, 0, b.count, &pairs)
    return pairs
}

private func align(
    _ a: [String],
    _ aStart: Int,
    _ aEnd: Int,
    _ b: [String],
    _ bStart: Int,
    _ bEnd: Int,
    _ out: inout [(Int, Int)]
) {
    if aEnd - aStart == 0 || bEnd - bStart == 0 { return }

    if aEnd - aStart == 1 {
        for j in bStart..<bEnd where a[aStart] == b[j] {
            out.append((aStart, j))
            return
        }
        return
    }

    let aMid = aStart + (aEnd - aStart) / 2
    let forward = lcsRow(a, aStart, aMid, b, bStart, bEnd, reversed: false)
    let backward = lcsRow(a, aMid, aEnd, b, bStart, bEnd, reversed: true)

    // Split the source where the two halves together match the most. Ties take the leftmost split
    // so both platform implementations choose the same alignment among equally long ones.
    var bestScore = -1
    var bestSplit = bStart
    for j in bStart...bEnd {
        let score = Int(forward[j - bStart]) + Int(backward[bEnd - j])
        if score > bestScore {
            bestScore = score
            bestSplit = j
        }
    }

    align(a, aStart, aMid, b, bStart, bestSplit, &out)
    align(a, aMid, aEnd, b, bestSplit, bEnd, &out)
}

/// Final DP row of the LCS lengths for one half, walked forwards or backwards.
private func lcsRow(
    _ a: [String],
    _ aStart: Int,
    _ aEnd: Int,
    _ b: [String],
    _ bStart: Int,
    _ bEnd: Int,
    reversed: Bool
) -> [UInt32] {
    let width = bEnd - bStart + 1
    var previous = [UInt32](repeating: 0, count: width)
    var current = [UInt32](repeating: 0, count: width)

    for i in aStart..<aEnd {
        let left = reversed ? a[aEnd - 1 - (i - aStart)] : a[i]
        current[0] = 0
        for j in 1..<width {
            let right = reversed ? b[bEnd - j] : b[bStart + j - 1]
            current[j] = left == right ? previous[j - 1] + 1 : max(previous[j], current[j - 1])
        }
        swap(&previous, &current)
    }

    return previous
}

