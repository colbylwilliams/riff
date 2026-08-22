import Foundation

public struct Utterance: Codable, Sendable, Hashable {
    public enum Source: String, Codable, Sendable { case speech, typed }
    public var id: String
    /// Exactly what the transcriber produced, before any correction.
    public var text: String
    public var at: String
    public var source: Source
    public var confidence: Double?
}

/// Everything the speaker said, in order, as the transcriber produced it.
///
/// This is the only source the prompt body may draw from. It is append-and-revise: a transcript can
/// be corrected while it is still streaming, but nothing is ever removed, because a superseded
/// sentence still has to be provable as theirs if it turns out they meant it after all.
public final class UtteranceLedger: @unchecked Sendable {
    private var utterances: [Utterance] = []
    private var byId: [String: Int] = [:]
    private var sequence = 0
    private var cachedSpans: [SourceSpan]?
    private let lexicon: Lexicon
    private let windowSize: Int
    private let redact: Bool
    private let lock = NSLock()

    public init(lexicon: Lexicon, windowSize: Int, redact: Bool = true) {
        self.lexicon = lexicon
        self.windowSize = windowSize
        self.redact = redact
    }

    public var count: Int {
        lock.lock(); defer { lock.unlock() }
        return utterances.count
    }

    @discardableResult
    public func append(text: String, at: String = ISO8601.now(), source: Utterance.Source = .speech, confidence: Double? = nil) -> Utterance {
        let cleaned = RiffText.tidyWhitespace(redact ? redactSecrets(text) : text)
        lock.lock(); defer { lock.unlock() }
        sequence += 1
        let utterance = Utterance(id: "u\(sequence)", text: cleaned, at: at, source: source, confidence: confidence)
        byId[utterance.id] = utterances.count
        utterances.append(utterance)
        cachedSpans = nil
        return utterance
    }

    /// Replaces the text of an utterance when a streaming transcript is finalized or corrected.
    @discardableResult
    public func revise(id: String, text: String) -> Utterance? {
        lock.lock(); defer { lock.unlock() }
        guard let index = byId[id] else { return nil }
        utterances[index].text = RiffText.tidyWhitespace(redact ? redactSecrets(text) : text)
        cachedSpans = nil
        return utterances[index]
    }

    public func all() -> [Utterance] {
        lock.lock(); defer { lock.unlock() }
        return utterances
    }

    /// Call when the lexicon changes, so cached spans pick up the new corrections.
    public func invalidate() {
        lock.lock(); defer { lock.unlock() }
        cachedSpans = nil
    }

    /// Every window of up to `windowSize` consecutive utterances, longest first. Windows exist
    /// because transcription splits on pauses rather than on sentences, so one spoken sentence
    /// routinely arrives as two or three utterances.
    public func spans() -> [SourceSpan] {
        lock.lock()
        if let cachedSpans { lock.unlock(); return cachedSpans }
        let snapshot = utterances
        lock.unlock()

        var tokenCache: [String: (tokens: [String], rawTokens: [String])] = [:]
        func tokens(for utterance: Utterance) -> (tokens: [String], rawTokens: [String]) {
            if let cached = tokenCache[utterance.id] { return cached }
            let rawTokens = RiffText.tokenize(utterance.text)
            let value = (lexicon.canonicalize(rawTokens).tokens, rawTokens)
            tokenCache[utterance.id] = value
            return value
        }

        var spans: [SourceSpan] = []
        var width = min(windowSize, snapshot.count)
        while width >= 1 {
            var start = 0
            while start + width <= snapshot.count {
                let members = Array(snapshot[start..<(start + width)])
                var tokenList: [String] = []
                var owners: [String] = []
                var rawTokens: [String] = []
                for member in members {
                    let value = tokens(for: member)
                    tokenList.append(contentsOf: value.tokens)
                    owners.append(contentsOf: Array(repeating: member.id, count: value.tokens.count))
                    rawTokens.append(contentsOf: value.rawTokens)
                }
                spans.append(SourceSpan(
                    utteranceIds: members.map(\.id),
                    tokens: tokenList,
                    owners: owners,
                    rawTokens: rawTokens
                ))
                start += 1
            }
            width -= 1
        }

        lock.lock()
        cachedSpans = spans
        lock.unlock()
        return spans
    }

    /// Recent transcript, for the gist readback and for host tools that need conversational context.
    public func recentText(count: Int = 8) -> String {
        all().suffix(count).map(\.text).joined(separator: " ")
    }
}

public enum ISO8601 {
    // A fresh formatter per call: ISO8601DateFormatter is not Sendable, and timestamps are written
    // once per utterance rather than in a hot loop.
    private static func formatter() -> ISO8601DateFormatter {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return formatter
    }

    public static func now() -> String { string(from: Date()) }
    public static func string(from date: Date) -> String { formatter().string(from: date) }
}
