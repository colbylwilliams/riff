/**
 * A RiffHost with a fixed little world in it.
 *
 * The host is where everything world-shaped lives, so this is the file you would replace with
 * `GitHubHost` to point the demo at a real repository. Keeping it canned means "the PR I just
 * opened" resolves the same way every time, which is what you want when someone is watching.
 *
 * `submitPrompt` is the honest part: it accepts the artifact, reports success, and sends it nowhere.
 * The whole pipeline up to that point is real — the ledger, the grounding check, the render — so the
 * prompt on screen is the prompt that would have been delivered.
 */
export class DemoHost {
  /** Artifacts this host "received", newest first. */
  submitted = [];

  async environment() {
    return {
      workspace: "acme/web",
      repository: "acme/web",
      branch: "main",
      user: { login: "you", name: "You" },
      destinations: [{ id: "demo", label: "Demo destination (goes nowhere)", default: true }],
      vocabulary: [
        { canonical: "GitHub", kind: "product", heardAs: ["get hub", "git hub"] },
        { canonical: "acme/web", kind: "repository", heardAs: ["acme web", "acmy web"] },
      ],
      recent: [PULL_REQUEST],
    };
  }

  async resolveReference({ phrase }) {
    // Deliberately narrow. A host that guesses sends the downstream agent somewhere real and wrong,
    // which is worse than an unresolved reference the agent asks about.
    if (/\bpr\b|pull request/i.test(phrase)) return { candidates: [{ ...PULL_REQUEST, confidence: 0.94 }] };
    return { candidates: [] };
  }

  async lookupTerm({ heard }) {
    const match = TERMS.find(
      (term) =>
        term.canonical.toLowerCase() === heard.toLowerCase() ||
        term.heardAs?.some((alias) => alias.toLowerCase() === heard.toLowerCase()),
    );
    return { matches: match ? [{ ...match, confidence: 0.9 }] : [] };
  }

  async recallPrompts() {
    return { prompts: [] };
  }

  async submitPrompt(artifact, options = {}) {
    this.submitted.unshift(artifact);
    return {
      submitted: true,
      promptId: artifact.id,
      destination: options.target ?? "demo",
      message: "accepted by the demo host; nothing left this machine",
    };
  }
}

const PULL_REQUEST = {
  referenceId: "ref-pr-412",
  kind: "pull_request",
  title: "Chunked uploads",
  identifier: "acme/web#412",
  url: "https://github.com/acme/web/pull/412",
  state: "open",
  timestamp: "2025-01-14T17:02:00Z",
};

const TERMS = [
  { canonical: "GitHub", kind: "product", heardAs: ["get hub", "git hub"] },
  { canonical: "acme/web", kind: "repository", heardAs: ["acme web"] },
];

/**
 * The standing instruction the speaker saved at some earlier point.
 *
 * Seeded into the store so the demo can show a motif being reattached in their own words rather
 * than a constraint the agent invented on the spot.
 */
export const DEMO_MOTIFS = [
  {
    id: "m-generated-files",
    text: "don't touch the generated files",
    scope: "user",
    appliesWhen: "changes that touch a repository with generated output",
    createdAt: "2025-01-02T09:00:00Z",
  },
];
