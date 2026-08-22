import type { ContextItem } from "@riff/core";

export type FetchLike = (url: string, init?: Record<string, unknown>) => Promise<Response>;

export interface GitHubClientOptions {
  token: string;
  baseUrl?: string;
  fetch?: FetchLike;
  userAgent?: string;
}

/** Thin REST client. Only the handful of endpoints reference resolution needs. */
export class GitHubClient {
  readonly #token: string;
  readonly #baseUrl: string;
  readonly #fetch: FetchLike;
  readonly #userAgent: string;

  constructor(options: GitHubClientOptions) {
    this.#token = options.token;
    this.#baseUrl = (options.baseUrl ?? "https://api.github.com").replace(/\/$/, "");
    this.#fetch = options.fetch ?? ((url, init) => fetch(url, init as RequestInit));
    this.#userAgent = options.userAgent ?? "riff";
  }

  async get<T>(path: string, query: Record<string, string | number | undefined> = {}): Promise<T> {
    const url = new URL(`${this.#baseUrl}${path}`);
    for (const [key, value] of Object.entries(query)) {
      if (value !== undefined) url.searchParams.set(key, String(value));
    }

    const response = await this.#fetch(url.toString(), {
      headers: {
        Authorization: `Bearer ${this.#token}`,
        Accept: "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": this.#userAgent,
      },
    });

    if (!response.ok) {
      throw new Error(`GitHub ${path} failed (${response.status}): ${(await safeText(response)).slice(0, 200)}`);
    }
    return (await response.json()) as T;
  }

  async post<T>(path: string, body: unknown): Promise<T> {
    const response = await this.#fetch(`${this.#baseUrl}${path}`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${this.#token}`,
        Accept: "application/vnd.github+json",
        "Content-Type": "application/json",
        "X-GitHub-Api-Version": "2022-11-28",
        "User-Agent": this.#userAgent,
      },
      body: JSON.stringify(body),
    });

    if (!response.ok) {
      throw new Error(`GitHub ${path} failed (${response.status}): ${(await safeText(response)).slice(0, 200)}`);
    }
    return (await response.json()) as T;
  }
}

async function safeText(response: Response): Promise<string> {
  try {
    return await response.text();
  } catch {
    return "(no body)";
  }
}

export interface IssueLike {
  number: number;
  title: string;
  html_url: string;
  state: string;
  draft?: boolean;
  updated_at?: string;
  created_at?: string;
  user?: { login?: string };
  pull_request?: unknown;
  repository_url?: string;
  body?: string;
}

/** Maps a GitHub issue or pull request onto the neutral context shape Riff attaches to prompts. */
export function toContextItem(issue: IssueLike, repository?: string): ContextItem {
  const isPullRequest = Boolean(issue.pull_request);
  const repo = repository ?? issue.repository_url?.replace(/^.*\/repos\//, "");
  const identifier = repo ? `${repo}#${issue.number}` : `#${issue.number}`;

  return {
    referenceId: `${isPullRequest ? "pr" : "issue"}-${repo ?? "unknown"}-${issue.number}`,
    kind: isPullRequest ? "pull_request" : "issue",
    title: issue.title,
    identifier,
    url: issue.html_url,
    ...(issue.user?.login ? { actor: issue.user.login } : {}),
    ...(issue.updated_at ? { timestamp: issue.updated_at } : {}),
    state: issue.draft ? "draft" : issue.state,
  };
}
