import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, it } from "node:test";

import { createGroundingChecker } from "../src/grounding.ts";
import { Lexicon } from "../src/lexicon.ts";
import { UtteranceLedger } from "../src/ledger.ts";
import { Take } from "../src/draft.ts";
import { renderPrompt } from "../src/render.ts";
import { loadBundle } from "../src/bundle.ts";
import type { AgentBundle, GroundingConfig, LexiconTerm, Section } from "../src/types.ts";

const root = new URL("../../../", import.meta.url).pathname;
const bundle: AgentBundle = loadBundle(
  JSON.parse(readFileSync(join(root, "core/dist/riff-agent.bundle.json"), "utf8")),
);

const readCases = (name: string) =>
  JSON.parse(readFileSync(join(root, "core/conformance/cases", name), "utf8"));

interface GroundingCase {
  id: string;
  description: string;
  utterances: string[];
  lexicon?: LexiconTerm[];
  candidate: string;
  mode?: "line" | "title";
  expect: {
    ok: boolean;
    kind?: string;
    ratio?: number;
    unmatchedIncludes?: string[];
    sourceUtterances?: string[];
  };
}

describe("conformance: grounding", () => {
  const suite = readCases("grounding.json") as { defaults: Partial<GroundingConfig>; cases: GroundingCase[] };

  it("uses the same thresholds as the shipped bundle", () => {
    assert.equal(suite.defaults.threshold, bundle.grounding.threshold);
    assert.equal(suite.defaults.titleThreshold, bundle.grounding.titleThreshold);
    assert.equal(suite.defaults.windowSize, bundle.grounding.windowSize);
  });

  for (const testCase of suite.cases) {
    it(`${testCase.id}: ${testCase.description}`, () => {
      const lexicon = new Lexicon(testCase.lexicon ?? []);
      const ledger = new UtteranceLedger(lexicon, bundle.grounding.windowSize, false);
      for (const text of testCase.utterances) ledger.append({ text, source: "speech" });

      const checker = createGroundingChecker(bundle.grounding, lexicon);
      const result =
        testCase.mode === "title"
          ? checker.checkTitle(testCase.candidate, ledger.spans())
          : checker.check(testCase.candidate, ledger.spans());

      assert.equal(result.ok, testCase.expect.ok, `ok mismatch (ratio ${result.ratio}, kind ${result.kind})`);

      if (testCase.expect.ratio !== undefined) {
        assert.equal(result.ratio, testCase.expect.ratio);
      }
      if (testCase.expect.kind !== undefined) {
        assert.equal(result.kind, testCase.expect.kind);
      }
      for (const token of testCase.expect.unmatchedIncludes ?? []) {
        assert.ok(
          result.unmatchedTokens.includes(token),
          `expected "${token}" to be reported as not said, got [${result.unmatchedTokens.join(", ")}]`,
        );
      }
      if (testCase.expect.sourceUtterances) {
        assert.deepEqual(result.sourceUtteranceIds.sort(), testCase.expect.sourceUtterances.slice().sort());
      }
    });
  }
});

interface RenderCase {
  id: string;
  description: string;
  profile: string;
  take: {
    title?: string;
    lines: Array<{ section: Section; text: string }>;
    context?: Array<Record<string, string>>;
  };
  expect: string;
}

describe("conformance: rendering", () => {
  const suite = readCases("render.json") as { cases: RenderCase[] };

  for (const testCase of suite.cases) {
    it(`${testCase.id}: ${testCase.description}`, () => {
      const take = new Take("t1", "2026-01-01T00:00:00.000Z");
      if (testCase.take.title) take.title = { text: testCase.take.title, origin: "spoken" };

      for (const line of testCase.take.lines) {
        take.setLine({
          id: take.nextLineId(),
          section: line.section,
          text: line.text,
          order: take.orderAfter(line.section, undefined),
          sourceUtteranceIds: [],
          grounding: { ratio: 1, kind: "verbatim" },
        });
      }
      for (const item of testCase.take.context ?? []) {
        take.attachContext(item as never);
      }

      assert.equal(renderPrompt(take, { config: bundle.render, profile: testCase.profile }), testCase.expect);
    });
  }
});

interface LexiconCase {
  id: string;
  description: string;
  terms: LexiconTerm[];
  limit?: number;
  expect: string[];
}

describe("conformance: biasing vocabulary", () => {
  const suite = readCases("lexicon.json") as { cases: LexiconCase[] };

  for (const testCase of suite.cases) {
    it(`${testCase.id}: ${testCase.description}`, () => {
      const lexicon = new Lexicon(testCase.terms);
      assert.deepEqual(lexicon.keywords(testCase.limit ?? 100), testCase.expect);
    });
  }
});
