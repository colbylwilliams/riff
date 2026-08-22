import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { GitHubHost, issueDestination } from "../src/host.ts";

interface Recorded {
  url: string;
  init?: any;
}

function fakeGitHub(routes: Record<string, unknown>): { fetch: any; calls: Recorded[] } {
  const calls: Recorded[] = [];
  const fetch = async (url: string, init?: any) => {
    calls.push({ url, init });
    const path = new URL(url).pathname;
    const body = routes[path];
    if (body === undefined) return new Response("not found", { status: 404 });
    return new Response(JSON.stringify(body), { status: 200 });
  };
  return { fetch, calls };
}

const pullRequest = {
  number: 412,
  title: "Chunked uploads",
  html_url: "https://github.com/acme/web/pull/412",
  state: "open",
  updated_at: "2026-08-20T10:00:00Z",
  user: { login: "colby" },
  pull_request: {},
};

describe("GitHubHost", () => {
  it("resolves a spoken shorthand exactly, with no search", async () => {
    const { fetch, calls } = fakeGitHub({ "/repos/acme/web/issues/412": pullRequest });
    const host = new GitHubHost({ token: "t", repository: "acme/web", fetch });

    const { candidates } = await host.resolveReference({ phrase: "go look at 412", kind: "pull_request" });

    assert.equal(candidates.length, 1);
    assert.equal(candidates[0]?.identifier, "acme/web#412");
    assert.equal(candidates[0]?.kind, "pull_request");
    assert.equal(candidates[0]?.confidence, 1);
    assert.equal(calls.length, 1);
  });

  it("resolves a pasted or spoken GitHub URL", async () => {
    const { fetch } = fakeGitHub({ "/repos/acme/web/issues/412": pullRequest });
    const host = new GitHubHost({ token: "t", fetch });

    const { candidates } = await host.resolveReference({
      phrase: "https://github.com/acme/web/pull/412 that one",
    });

    assert.equal(candidates[0]?.url, "https://github.com/acme/web/pull/412");
  });

  it("records a non-GitHub link without fetching it", async () => {
    const { fetch, calls } = fakeGitHub({});
    const host = new GitHubHost({ token: "t", fetch });

    const { candidates } = await host.resolveReference({ phrase: "the doc at https://example.com/spec" });

    assert.deepEqual(candidates, [
      { referenceId: "url-https://example.com/spec", kind: "url", title: "https://example.com/spec", url: "https://example.com/spec", confidence: 1 },
    ]);
    assert.equal(calls.length, 0);
  });

  it("turns 'the PR I just opened' into a recency scoped search, not a keyword search", async () => {
    const { fetch, calls } = fakeGitHub({ "/search/issues": { items: [pullRequest] } });
    const host = new GitHubHost({ token: "t", repository: "acme/web", fetch });

    const { candidates } = await host.resolveReference({
      phrase: "the PR I just opened",
      kind: "pull_request",
      recency: "latest",
      actor: "me",
    });

    const query = new URL(calls[0]!.url).searchParams.get("q")!;
    assert.match(query, /repo:acme\/web/);
    assert.match(query, /is:pr/);
    assert.match(query, /author:@me/);
    assert.match(query, /sort:updated-desc/);
    // The pointing words are not search terms; treating them as such returns nothing.
    assert.doesNotMatch(query, /opened|just|the/);
    assert.equal(candidates[0]?.confidence, 0.9);
  });

  it("keeps words that could match a title", async () => {
    const { fetch, calls } = fakeGitHub({ "/search/issues": { items: [] } });
    const host = new GitHubHost({ token: "t", repository: "acme/web", fetch });

    await host.resolveReference({ phrase: "that issue about the avatar caching", kind: "issue" });

    const query = new URL(calls[0]!.url).searchParams.get("q")!;
    assert.match(query, /avatar caching/);
  });

  it("reports the environment so references resolve without anyone being asked", async () => {
    const { fetch } = fakeGitHub({
      "/user": { login: "colby", name: "Colby" },
      "/search/issues": { items: [pullRequest] },
    });
    const host = new GitHubHost({
      token: "t",
      repository: "acme/web",
      branch: "main",
      fetch,
      destinations: [issueDestination({ repository: "acme/web", default: true })],
    });

    const environment = await host.environment();

    assert.equal(environment.repository, "acme/web");
    assert.equal(environment.user?.login, "colby");
    assert.equal(environment.recent?.[0]?.identifier, "acme/web#412");
    assert.deepEqual(environment.destinations, [
      { id: "issue", label: "New issue in acme/web", default: true },
    ]);
  });

  it("does nothing on submit when no destination is configured", async () => {
    const { fetch, calls } = fakeGitHub({});
    const host = new GitHubHost({ token: "t", fetch });

    const result = await host.submitPrompt({ id: "a1", rendered: "# hi" } as never, {});

    assert.equal(result.destination, "none");
    assert.match(result.message!, /not sent anywhere/);
    assert.equal(calls.length, 0);
  });

  it("opens an issue when that is the configured destination", async () => {
    const { fetch, calls } = fakeGitHub({
      "/repos/acme/web/issues": { number: 77, html_url: "https://github.com/acme/web/issues/77" },
    });
    const host = new GitHubHost({
      token: "t",
      fetch,
      destinations: [issueDestination({ repository: "acme/web", labels: ["riff"], default: true })],
    });

    const result = await host.submitPrompt(
      { id: "a1", title: { text: "Fix the uploader", origin: "derived" }, rendered: "# Fix the uploader\n\nbody" } as never,
      {},
    );

    assert.equal(result.submitted, true);
    assert.equal(result.url, "https://github.com/acme/web/issues/77");
    const body = JSON.parse(calls[0]!.init.body);
    assert.equal(body.title, "Fix the uploader");
    assert.deepEqual(body.labels, ["riff"]);
  });

  it("prefers configured workspace vocabulary over a search", async () => {
    const { fetch, calls } = fakeGitHub({});
    const host = new GitHubHost({
      token: "t",
      fetch,
      vocabulary: [{ canonical: "Flakeguard", kind: "service", heardAs: ["flake guard"], definition: "retry wrapper" }],
    });

    const { matches } = await host.lookupTerm({ heard: "flake guard" });

    assert.equal(matches[0]?.canonical, "Flakeguard");
    assert.equal(matches[0]?.definition, "retry wrapper");
    assert.equal(calls.length, 0);
  });
});

describe("spoken references", () => {
  it("does not mistake a quantity for an issue number", async () => {
    const { fetch, calls } = fakeGitHub({ "/search/issues": { items: [] } });
    const host = new GitHubHost({ token: "t", repository: "acme/web", fetch });

    await host.resolveReference({ phrase: "it breaks past 1000 rows" });

    assert.equal(new URL(calls[0]!.url).pathname, "/search/issues");
  });

  it("treats a bare number as an issue when the sentence says so", async () => {
    const { fetch, calls } = fakeGitHub({ "/repos/acme/web/issues/412": pullRequest });
    const host = new GitHubHost({ token: "t", repository: "acme/web", fetch });

    const { candidates } = await host.resolveReference({ phrase: "check the PR 412 diff" });

    assert.equal(candidates[0]?.identifier, "acme/web#412");
    assert.equal(new URL(calls[0]!.url).pathname, "/repos/acme/web/issues/412");
  });
});
