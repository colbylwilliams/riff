import type {
  ContextItem,
  HostEnvironment,
  LexiconTerm,
  LookupTermRequest,
  LookupTermResult,
  PromptArtifact,
  RecallPromptsRequest,
  RecallPromptsResult,
  ResolveReferenceRequest,
  ResolveReferenceResult,
  RiffHost,
  RiffStore,
  SubmitOptions,
  SubmitResult,
} from "@riff/core";
import type { GitHubClientOptions, IssueLike } from "./client.ts";
import { GitHubClient, toContextItem } from "./client.ts";

export interface SubmitDestination {
  id: string;
  label: string;
  default?: boolean;
  send(artifact: PromptArtifact, client: GitHubClient): Promise<SubmitResult>;
}

export interface GitHubHostOptions extends GitHubClientOptions {
  /** `owner/name` of the repository being worked in. Makes references resolvable without asking. */
  repository?: string;
  branch?: string;
  workspace?: string;
  /** Where finished prompts go. With none configured, submitting returns the prompt and does nothing. */
  destinations?: SubmitDestination[];
  /** Terms specific to this workspace, folded into the lexicon and into transcription biasing. */
  vocabulary?: LexiconTerm[];
  /** Backs `recall_prompts` with previously submitted artifacts. */
  store?: RiffStore;
}

const GITHUB_URL = /https?:\/\/(?:www\.)?github\.com\/([\w.-]+)\/([\w.-]+)\/(pull|issues|commit)\/([\w.-]+)/i;
const BARE_URL = /https?:\/\/\S+/;
const SHORTHAND = /(?:([\w.-]+)\/([\w.-]+))?#(\d+)/;
/** Nobody says "hash" out loud, so a spoken reference is a bare number next to a noun. */
const SPOKEN_NUMBER = /\b(?:pr|pull request|issue|ticket|bug)\s+(\d{1,6})\b|\b(\d{1,6})\b/i;
const NUMBER_NOUN = /\b(pr|prs|pull request|issue|issues|ticket|bug)\b/i;

/**
 * A host backed by GitHub.
 *
 * This is what turns "the PR I just opened" into `owner/repo#412` with a title and a URL, which is
 * the single most common piece of context someone would otherwise have to go and look up by hand
 * before they could finish writing a prompt.
 */
export class GitHubHost implements RiffHost {
  readonly #client: GitHubClient;
  readonly #options: GitHubHostOptions;

  constructor(options: GitHubHostOptions) {
    this.#options = options;
    this.#client = new GitHubClient(options);
  }

  async resolveReference(request: ResolveReferenceRequest): Promise<ResolveReferenceResult> {
    const direct = await this.#resolveDirect(request.phrase, request.kind);
    if (direct.length > 0) return { candidates: direct };

    const kind = request.kind ?? "unknown";
    if (kind === "person") return { candidates: await this.#searchUsers(request.phrase, request.limit ?? 5) };
    if (kind === "repository") return { candidates: await this.#searchRepositories(request.phrase, request.limit ?? 5) };

    return { candidates: await this.#searchIssues(request) };
  }

  async lookupTerm(request: LookupTermRequest): Promise<LookupTermResult> {
    const heard = request.heard.trim();
    if (heard.length === 0) return { matches: [] };

    const configured = (this.#options.vocabulary ?? []).filter(
      (term) =>
        term.canonical.toLowerCase() === heard.toLowerCase() ||
        term.heardAs?.some((alias) => alias.toLowerCase() === heard.toLowerCase()),
    );
    if (configured.length > 0) return { matches: configured.map((term) => ({ ...term, confidence: 1 })) };

    const matches: Array<LexiconTerm & { confidence?: number }> = [];

    try {
      const repositories = await this.#client.get<{ items: Array<{ full_name: string; description?: string }> }>(
        "/search/repositories",
        { q: `${heard} in:name`, per_page: 3 },
      );
      for (const repository of repositories.items ?? []) {
        matches.push({
          canonical: repository.full_name.split("/")[1] ?? repository.full_name,
          kind: "repository",
          ...(repository.description ? { definition: repository.description } : {}),
          confidence: 0.7,
        });
      }
    } catch {
      // Search is best effort; the agent asks when nothing comes back.
    }

    try {
      const users = await this.#client.get<{ items: Array<{ login: string; name?: string }> }>("/search/users", {
        q: `${heard} in:login in:name`,
        per_page: 3,
      });
      for (const user of users.items ?? []) {
        matches.push({
          canonical: user.login,
          kind: "person",
          ...(user.name ? { definition: user.name } : {}),
          confidence: 0.6,
        });
      }
    } catch {
      // As above.
    }

    return { matches };
  }

  async recallPrompts(request: RecallPromptsRequest): Promise<RecallPromptsResult> {
    const store = this.#options.store;
    if (!store) return { prompts: [] };

    const needle = request.query.toLowerCase();
    const artifacts = await store.listArtifacts({ limit: 100 });

    const prompts = artifacts
      .filter((artifact) => {
        if (request.status && request.status !== "any" && artifact.status !== request.status) return false;
        const haystack = `${artifact.title.text} ${artifact.rendered}`.toLowerCase();
        return needle.length === 0 || haystack.includes(needle);
      })
      .slice(0, request.limit ?? 5)
      .map((artifact) => ({
        promptId: artifact.id,
        title: artifact.title.text,
        excerpt: artifact.rendered.slice(0, 400),
        ...(artifact.updatedAt ? { submittedAt: artifact.updatedAt } : {}),
        ...(artifact.status ? { status: artifact.status } : {}),
      }));

    return { prompts };
  }

  async submitPrompt(artifact: PromptArtifact, options: SubmitOptions): Promise<SubmitResult> {
    const destinations = this.#options.destinations ?? [];
    const destination = options.target
      ? destinations.find((candidate) => candidate.id === options.target)
      : (destinations.find((candidate) => candidate.default) ?? destinations[0]);

    if (!destination) {
      return {
        submitted: true,
        promptId: artifact.id,
        destination: "none",
        message: "no destination configured; the prompt was produced but not sent anywhere",
      };
    }

    return destination.send(artifact, this.#client);
  }

  async environment(): Promise<HostEnvironment> {
    const environment: HostEnvironment = {
      ...(this.#options.repository ? { repository: this.#options.repository } : {}),
      ...(this.#options.branch ? { branch: this.#options.branch } : {}),
      ...(this.#options.workspace ? { workspace: this.#options.workspace } : {}),
      ...(this.#options.vocabulary ? { vocabulary: this.#options.vocabulary } : {}),
      destinations: (this.#options.destinations ?? []).map((destination) => ({
        id: destination.id,
        label: destination.label,
        ...(destination.default ? { default: true } : {}),
      })),
    };

    try {
      const user = await this.#client.get<{ login: string; name?: string }>("/user");
      environment.user = { login: user.login, ...(user.name ? { name: user.name } : {}) };
    } catch {
      // Anonymous or restricted tokens still give a usable session.
    }

    try {
      const recent = await this.#client.get<{ items: IssueLike[] }>("/search/issues", {
        q: `author:@me sort:updated-desc${this.#options.repository ? ` repo:${this.#options.repository}` : ""}`,
        per_page: 5,
        advanced_search: "true",
      });
      environment.recent = (recent.items ?? []).map((item) => toContextItem(item, this.#options.repository));
    } catch {
      // As above.
    }

    return environment;
  }

  /** URLs, `owner/repo#123`, and spoken numbers resolve exactly, with no search and no ambiguity. */
  async #resolveDirect(phrase: string, kind?: string): Promise<ContextItem[]> {
    const url = GITHUB_URL.exec(phrase);
    if (url) {
      const [, owner, repo, type, id] = url;
      const repository = `${owner}/${repo}`;
      if (type === "commit") {
        return [await this.#commit(repository, id!)];
      }
      return [await this.#issue(repository, Number(id))];
    }

    // Any other link is recorded and never mined for identifiers. A tracker URL is full of digits
    // and often a `#fragment`, so running the heuristics below over it turns someone else's link
    // into a confident lookup of an unrelated issue in this repository.
    const bareUrl = BARE_URL.exec(phrase);
    if (bareUrl) {
      // Deliberately not fetched: Riff does not pull arbitrary pages, it records the link.
      return [
        {
          referenceId: `url-${bareUrl[0]}`,
          kind: "url",
          title: bareUrl[0],
          url: bareUrl[0],
          confidence: 1,
        },
      ];
    }

    const shorthand = SHORTHAND.exec(phrase);
    if (shorthand) {
      const repository =
        shorthand[1] && shorthand[2] ? `${shorthand[1]}/${shorthand[2]}` : this.#options.repository;
      if (repository) return [await this.#issue(repository, Number(shorthand[3]))];
    }

    // "Go look at 412" only means a number when something in the sentence says it does, otherwise
    // every quantity anyone mentions would be mistaken for an issue.
    const numbered = SPOKEN_NUMBER.exec(phrase);
    const numberIsMeant =
      numbered && (Boolean(numbered[1]) || NUMBER_NOUN.test(phrase) || kind === "pull_request" || kind === "issue");
    if (numbered && numberIsMeant && this.#options.repository) {
      const number = Number(numbered[1] ?? numbered[2]);
      if (Number.isFinite(number) && number > 0) {
        return [await this.#issue(this.#options.repository, number)];
      }
    }

    return [];
  }

  async #issue(repository: string, number: number): Promise<ContextItem> {
    const issue = await this.#client.get<IssueLike>(`/repos/${repository}/issues/${number}`);
    return { ...toContextItem(issue, repository), confidence: 1 };
  }

  async #commit(repository: string, sha: string): Promise<ContextItem> {
    const commit = await this.#client.get<{
      sha: string;
      html_url: string;
      commit: { message: string; author?: { name?: string; date?: string } };
    }>(`/repos/${repository}/commits/${sha}`);

    return {
      referenceId: `commit-${repository}-${commit.sha.slice(0, 7)}`,
      kind: "commit",
      title: commit.commit.message.split("\n")[0] ?? commit.sha,
      identifier: `${repository}@${commit.sha.slice(0, 7)}`,
      url: commit.html_url,
      ...(commit.commit.author?.name ? { actor: commit.commit.author.name } : {}),
      ...(commit.commit.author?.date ? { timestamp: commit.commit.author.date } : {}),
      confidence: 1,
    };
  }

  async #searchIssues(request: ResolveReferenceRequest): Promise<ContextItem[]> {
    const qualifiers: string[] = [];
    if (this.#options.repository) qualifiers.push(`repo:${this.#options.repository}`);
    if (request.kind === "pull_request") qualifiers.push("is:pr");
    if (request.kind === "issue") qualifiers.push("is:issue");

    const actor = request.actor;
    if (actor) qualifiers.push(`author:${actor === "me" ? "@me" : actor}`);

    const since = sinceFor(request.recency);
    if (since) qualifiers.push(`updated:>=${since}`);

    // "The PR I just opened" is a recency claim, not a search term, so free text is only added when
    // the phrase carries words that could plausibly match a title.
    const freeText = meaningfulWords(request.phrase);
    const query = [...qualifiers, ...(freeText ? [freeText] : [])].join(" ").trim();
    if (query.length === 0) return [];

    const result = await this.#client.get<{ items: IssueLike[] }>("/search/issues", {
      q: `${query} sort:updated-desc`,
      per_page: request.limit ?? 5,
      advanced_search: "true",
    });

    return (result.items ?? []).map((item, index) => ({
      ...toContextItem(item, this.#options.repository),
      confidence: request.recency === "latest" && index === 0 ? 0.9 : 0.6,
    }));
  }

  async #searchRepositories(phrase: string, limit: number): Promise<ContextItem[]> {
    const result = await this.#client.get<{
      items: Array<{ full_name: string; html_url: string; description?: string; updated_at?: string }>;
    }>("/search/repositories", { q: `${meaningfulWords(phrase) || phrase} in:name`, per_page: limit });

    return (result.items ?? []).map((repository) => ({
      referenceId: `repo-${repository.full_name}`,
      kind: "repository",
      title: repository.full_name,
      identifier: repository.full_name,
      url: repository.html_url,
      ...(repository.description ? { summary: repository.description } : {}),
      ...(repository.updated_at ? { timestamp: repository.updated_at } : {}),
      confidence: 0.6,
    }));
  }

  async #searchUsers(phrase: string, limit: number): Promise<ContextItem[]> {
    const result = await this.#client.get<{ items: Array<{ login: string; html_url: string; name?: string }> }>(
      "/search/users",
      { q: meaningfulWords(phrase) || phrase, per_page: limit },
    );

    return (result.items ?? []).map((user) => ({
      referenceId: `user-${user.login}`,
      kind: "person",
      title: user.name ?? user.login,
      identifier: user.login,
      url: user.html_url,
      confidence: 0.6,
    }));
  }
}

const REFERRING_WORDS = new Set([
  "the", "that", "this", "a", "an", "one", "i", "my", "me", "just", "opened", "open", "filed",
  "made", "created", "wrote", "last", "latest", "recent", "yesterday", "today", "earlier",
  "pr", "prs", "pull", "request", "issue", "issues", "ticket", "commit", "thing", "it", "about",
  "from", "on", "in", "of", "for", "and", "we", "you", "he", "she", "they",
]);

/** Strips the pointing words out of a phrase, leaving anything that could match a title. */
function meaningfulWords(phrase: string): string {
  return phrase
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s-]/gu, " ")
    .split(/\s+/)
    .filter((word) => word.length > 2 && !REFERRING_WORDS.has(word))
    .slice(0, 6)
    .join(" ");
}

function sinceFor(recency: ResolveReferenceRequest["recency"]): string | null {
  const windows: Record<string, number> = { today: 1, this_week: 7, this_month: 31 };
  const days = windows[recency ?? "any"];
  if (!days) return null;
  return new Date(Date.now() - days * 86_400_000).toISOString().slice(0, 10);
}

/** Sends the prompt as a new issue, optionally assigned so an agent picks it up. */
export function issueDestination(options: {
  id?: string;
  label?: string;
  repository: string;
  labels?: string[];
  assignees?: string[];
  default?: boolean;
}): SubmitDestination {
  return {
    id: options.id ?? "issue",
    label: options.label ?? `New issue in ${options.repository}`,
    ...(options.default ? { default: true } : {}),
    async send(artifact, client) {
      const issue = await client.post<{ number: number; html_url: string }>(
        `/repos/${options.repository}/issues`,
        {
          title: artifact.title.text,
          body: artifact.rendered,
          ...(options.labels ? { labels: options.labels } : {}),
          ...(options.assignees ? { assignees: options.assignees } : {}),
        },
      );
      return {
        submitted: true,
        promptId: `${options.repository}#${issue.number}`,
        destination: options.id ?? "issue",
        url: issue.html_url,
      };
    },
  };
}
