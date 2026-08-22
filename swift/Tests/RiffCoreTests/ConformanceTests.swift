import Foundation
import Testing
@testable import RiffCore

/// Runs the language-neutral conformance suite in `core/conformance`.
///
/// The TypeScript engine runs the identical cases. That is what makes "the same agent everywhere" a
/// checkable claim rather than an aspiration: a prompt captured on a phone and one captured at a
/// desk are held to the same standard and render to the same bytes.
struct ConformanceTests {
    static let bundle = try! AgentBundle.bundled()

    static func load(_ name: String) throws -> JSONValue {
        guard let url = Bundle.module.url(
            forResource: (name as NSString).deletingPathExtension,
            withExtension: "json",
            subdirectory: "Resources"
        ) else {
            throw RiffError.bundleInvalid("conformance case file \(name) is missing from the test bundle")
        }
        return try JSONDecoder().decode(JSONValue.self, from: Data(contentsOf: url))
    }

    // MARK: - Grounding

    struct GroundingCase: CustomStringConvertible {
        var id: String
        var detail: String
        var utterances: [String]
        var lexicon: [LexiconTerm]
        var candidate: String
        var isTitle: Bool
        var expectOk: Bool
        var expectKind: String?
        var expectRatio: Double?
        var unmatchedIncludes: [String]
        var sourceUtterances: [String]?

        var description: String { "\(id): \(detail)" }
    }

    static let groundingCases: [GroundingCase] = {
        let suite = try! load("grounding.json")
        return (suite["cases"]?.arrayValue ?? []).map { entry in
            GroundingCase(
                id: entry["id"]?.stringValue ?? "",
                detail: entry["description"]?.stringValue ?? "",
                utterances: (entry["utterances"]?.arrayValue ?? []).compactMap(\.stringValue),
                lexicon: (entry["lexicon"]?.arrayValue ?? []).map { term in
                    LexiconTerm(
                        canonical: term["canonical"]?.stringValue ?? "",
                        kind: term["kind"]?.stringValue ?? "other",
                        heardAs: term["heardAs"]?.arrayValue?.compactMap(\.stringValue)
                    )
                },
                candidate: entry["candidate"]?.stringValue ?? "",
                isTitle: entry["mode"]?.stringValue == "title",
                expectOk: entry["expect"]?["ok"]?.boolValue ?? false,
                expectKind: entry["expect"]?["kind"]?.stringValue,
                expectRatio: entry["expect"]?["ratio"]?.numberValue,
                unmatchedIncludes: entry["expect"]?["unmatchedIncludes"]?.arrayValue?.compactMap(\.stringValue) ?? [],
                sourceUtterances: entry["expect"]?["sourceUtterances"]?.arrayValue?.compactMap(\.stringValue)
            )
        }
    }()

    @Test("the suite is pinned to the thresholds the agent actually ships with")
    func thresholdsMatchTheBundle() throws {
        let defaults = try Self.load("grounding.json")["defaults"]
        #expect(defaults?["threshold"]?.numberValue == Self.bundle.grounding.threshold)
        #expect(defaults?["titleThreshold"]?.numberValue == Self.bundle.grounding.titleThreshold)
        #expect(defaults?["windowSize"]?.intValue == Self.bundle.grounding.windowSize)
    }

    @Test("grounding", arguments: groundingCases)
    func grounding(_ testCase: GroundingCase) {
        let lexicon = Lexicon(testCase.lexicon)
        let ledger = UtteranceLedger(
            lexicon: lexicon,
            windowSize: Self.bundle.grounding.windowSize,
            redact: false
        )
        for text in testCase.utterances { ledger.append(text: text) }

        let checker = GroundingChecker(config: Self.bundle.grounding, lexicon: lexicon)
        let spans = ledger.spans()
        let result = testCase.isTitle
            ? checker.checkTitle(testCase.candidate, against: spans)
            : checker.check(testCase.candidate, against: spans)

        #expect(result.ok == testCase.expectOk, "ratio \(result.ratio), kind \(result.kind.rawValue)")
        if let ratio = testCase.expectRatio { #expect(result.ratio == ratio) }
        if let kind = testCase.expectKind { #expect(result.kind.rawValue == kind) }
        for token in testCase.unmatchedIncludes {
            #expect(result.unmatchedTokens.contains(token), "expected \"\(token)\" to be reported as not said")
        }
        if let sources = testCase.sourceUtterances {
            #expect(result.sourceUtteranceIds.sorted() == sources.sorted())
        }
    }

    // MARK: - Rendering

    struct RenderCase: CustomStringConvertible {
        var id: String
        var detail: String
        var profile: String
        var title: String?
        var lines: [(Section, String)]
        var context: [ContextItem]
        var expected: String

        var description: String { "\(id): \(detail)" }
    }

    static let renderCases: [RenderCase] = {
        let suite = try! load("render.json")
        return (suite["cases"]?.arrayValue ?? []).map { entry in
            let take = entry["take"]
            return RenderCase(
                id: entry["id"]?.stringValue ?? "",
                detail: entry["description"]?.stringValue ?? "",
                profile: entry["profile"]?.stringValue ?? "prose",
                title: take?["title"]?.stringValue,
                lines: (take?["lines"]?.arrayValue ?? []).compactMap { line in
                    guard
                        let raw = line["section"]?.stringValue,
                        let section = Section(rawValue: raw),
                        let text = line["text"]?.stringValue
                    else { return nil }
                    return (section, text)
                },
                context: (take?["context"]?.arrayValue ?? []).map { item in
                    ContextItem(
                        referenceId: item["referenceId"]?.stringValue ?? "",
                        kind: item["kind"]?.stringValue ?? "",
                        title: item["title"]?.stringValue ?? "",
                        identifier: item["identifier"]?.stringValue,
                        url: item["url"]?.stringValue,
                        actor: item["actor"]?.stringValue,
                        state: item["state"]?.stringValue,
                        resolvedFrom: item["resolvedFrom"]?.stringValue
                    )
                },
                expected: entry["expect"]?.stringValue ?? ""
            )
        }
    }()

    @Test("rendering", arguments: renderCases)
    func rendering(_ testCase: RenderCase) throws {
        let take = Take(id: "t1", createdAt: "2026-01-01T00:00:00.000Z")
        if let title = testCase.title { take.title = (title, "spoken") }

        for (section, text) in testCase.lines {
            take.setLine(Line(
                id: take.nextLineId(),
                section: section,
                text: text,
                order: take.order(in: section, after: nil),
                sourceUtteranceIds: [],
                motifId: nil,
                supersedes: nil,
                grounding: Line.Grounding(ratio: 1, kind: .verbatim)
            ))
        }
        for item in testCase.context { take.attach(item) }

        let rendered = try renderPrompt(
            take,
            options: RenderOptions(config: Self.bundle.render, profile: testCase.profile)
        )
        #expect(rendered == testCase.expected)
    }
}
