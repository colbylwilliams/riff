/**
 * A RiffHost with a fixed little world in it.
 *
 * Slack, session history, and GitHub results are fixtures, so the scripted conversation is
 * reproducible without credentials or a network. Live mode never uses this host.
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

  async resolveReference({ phrase, kind }) {
    // Deliberately narrow. A host that guesses sends the downstream agent somewhere real and wrong,
    // which is worse than an unresolved reference the agent asks about.
    if ((!kind || kind === "message") && /\bslack thread from monday\b/i.test(phrase)) {
      return { candidates: [{ ...SLACK_THREAD, confidence: 1 }] };
    }
    if ((!kind || kind === "document") && /\blocal-drafts session\b/i.test(phrase)) {
      return {
        candidates: [{
          referenceId: "ref-session-local-drafts",
          kind: "document",
          identifier: LOCAL_DRAFTS.promptId,
          title: LOCAL_DRAFTS.title,
          url: LOCAL_DRAFTS.url,
          confidence: 1,
        }],
      };
    }
    if ((!kind || kind === "pull_request") && /\bpr\b|pull request/i.test(phrase)) {
      return { candidates: [{ ...PULL_REQUEST, confidence: 0.94 }] };
    }
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

  async recallPrompts({ query, limit = 5 }) {
    const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
    const prompts = PRIOR_PROMPTS.filter((prompt) => {
      const text = `${prompt.title} ${prompt.excerpt}`.toLowerCase();
      return terms.every((term) => text.includes(term));
    });
    return { prompts: prompts.slice(0, limit).map((prompt) => ({ ...prompt })) };
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
  title: "Save drafts locally",
  identifier: "acme/web#412",
  url: "https://github.com/acme/web/pull/412",
  state: "open",
  timestamp: "2026-09-08T17:02:00Z",
};

const SLACK_THREAD = {
  referenceId: "ref-slack-offline-drafts",
  kind: "message",
  identifier: "Slack #feedback",
  title: "Drafts lost on the train",
  url: "https://example.com/slack/offline-drafts",
  timestamp: "2026-09-07T15:30:00Z",
};

const LOCAL_DRAFTS = {
  promptId: "session-local-drafts",
  title: "Local drafts",
  excerpt: "Try keeping offline drafts in IndexedDB.",
  outcome: "The prototype stores snapshots; restoring them after reopening is unfinished.",
  submittedAt: "2026-09-02T10:00:00Z",
  status: "submitted",
  url: "https://example.com/sessions/local-drafts",
};

const PRIOR_PROMPTS = [
  LOCAL_DRAFTS,
  {
    promptId: "session-sync-retries",
    title: "Sync retries",
    excerpt: "Handle sync retries for offline drafts after a connection returns.",
    outcome: "Retry backoff is implemented; keeping local edits is a separate task.",
    submittedAt: "2026-09-01T14:00:00Z",
    status: "submitted",
    url: "https://example.com/sessions/sync-retries",
  },
];

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
