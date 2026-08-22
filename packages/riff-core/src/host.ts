import type { ContextItem, LexiconTerm, Motif, PromptArtifact } from "./types.ts";

/**
 * What the embedding application supplies.
 *
 * Riff knows how to keep a prompt in someone's voice; it does not know what their world contains.
 * Everything world shaped — which PR they just opened, what an acronym means here, what they asked
 * for last week, where a finished prompt goes — comes through this interface, which is why the same
 * agent works in an editor, a terminal, a phone, and a design tool without knowing the difference.
 */
export interface RiffHost {
  /** Turns "the PR I just opened" into a thing with an identifier and a URL. */
  resolveReference(request: ResolveReferenceRequest): Promise<ResolveReferenceResult>;
  /** Says what a term means here and how it is spelled. */
  lookupTerm(request: LookupTermRequest): Promise<LookupTermResult>;
  /** Finds prompts the speaker wrote before. */
  recallPrompts(request: RecallPromptsRequest): Promise<RecallPromptsResult>;
  /** Hands the finished prompt to whatever does the work. */
  submitPrompt(artifact: PromptArtifact, options: SubmitOptions): Promise<SubmitResult>;
  /** Ambient facts that make references resolvable without asking. */
  environment?(): Promise<HostEnvironment>;
}

export interface ResolveReferenceRequest {
  phrase: string;
  kind?: string;
  recency?: "latest" | "today" | "this_week" | "this_month" | "any";
  actor?: string;
  limit?: number;
  /** Recent transcript, so a host can disambiguate from what was being discussed. */
  transcript?: string;
}

export interface ResolveReferenceResult {
  candidates: ContextItem[];
}

export interface LookupTermRequest {
  heard: string;
  context?: string;
  kind?: string;
}

export interface LookupTermResult {
  matches: Array<LexiconTerm & { confidence?: number }>;
}

export interface RecallPromptsRequest {
  query: string;
  recency?: "latest" | "today" | "this_week" | "this_month" | "any";
  status?: "any" | "submitted" | "parked";
  limit?: number;
}

export interface PriorPrompt {
  promptId: string;
  title: string;
  excerpt: string;
  submittedAt?: string;
  status?: string;
  outcome?: string;
  url?: string;
}

export interface RecallPromptsResult {
  prompts: PriorPrompt[];
}

export interface SubmitOptions {
  target?: string;
  keepOpen?: boolean;
}

export interface SubmitResult {
  submitted: boolean;
  promptId?: string;
  destination?: string;
  url?: string;
  message?: string;
}

export interface HostEnvironment {
  /** Where the speaker is working, phrased as they would say it. */
  workspace?: string;
  repository?: string;
  branch?: string;
  user?: { login?: string; name?: string };
  /** Destinations `submit_prompt` may target. */
  destinations?: Array<{ id: string; label: string; default?: boolean }>;
  /** Names, repos, and jargon specific to this workspace, folded into the lexicon at connect time. */
  vocabulary?: LexiconTerm[];
  /** Things recently touched, which make "the one I just opened" resolvable. */
  recent?: ContextItem[];
}

/**
 * A host for when there is nothing to look things up in.
 *
 * It resolves nothing rather than guessing, because a fabricated PR number sends the downstream
 * agent somewhere real and wrong, which is worse than an unresolved reference the agent asks about.
 */
export class NullHost implements RiffHost {
  async resolveReference(): Promise<ResolveReferenceResult> {
    return { candidates: [] };
  }
  async lookupTerm(): Promise<LookupTermResult> {
    return { matches: [] };
  }
  async recallPrompts(): Promise<RecallPromptsResult> {
    return { prompts: [] };
  }
  async submitPrompt(artifact: PromptArtifact): Promise<SubmitResult> {
    return { submitted: true, promptId: artifact.id, destination: "none" };
  }
}

/** Persistence for the things that outlive a session. */
export interface RiffStore {
  loadLexicon(): Promise<LexiconTerm[]>;
  saveTerm(term: LexiconTerm): Promise<void>;
  listMotifs(): Promise<Motif[]>;
  saveMotif(motif: Motif): Promise<void>;
  retireMotif(id: string, at: string): Promise<void>;
  saveArtifact(artifact: PromptArtifact): Promise<void>;
  listArtifacts(query?: { limit?: number }): Promise<PromptArtifact[]>;
}

export class MemoryStore implements RiffStore {
  #terms: LexiconTerm[] = [];
  #motifs = new Map<string, Motif>();
  #artifacts: PromptArtifact[] = [];

  constructor(seed: { terms?: LexiconTerm[]; motifs?: Motif[] } = {}) {
    this.#terms = [...(seed.terms ?? [])];
    for (const motif of seed.motifs ?? []) this.#motifs.set(motif.id, motif);
  }

  async loadLexicon(): Promise<LexiconTerm[]> {
    return [...this.#terms];
  }

  async saveTerm(term: LexiconTerm): Promise<void> {
    const index = this.#terms.findIndex((t) => t.canonical.toLowerCase() === term.canonical.toLowerCase());
    if (index === -1) this.#terms.push(term);
    else this.#terms[index] = term;
  }

  async listMotifs(): Promise<Motif[]> {
    return [...this.#motifs.values()].filter((motif) => !motif.retiredAt);
  }

  async saveMotif(motif: Motif): Promise<void> {
    this.#motifs.set(motif.id, motif);
  }

  async retireMotif(id: string, at: string): Promise<void> {
    const motif = this.#motifs.get(id);
    if (motif) this.#motifs.set(id, { ...motif, retiredAt: at });
  }

  async saveArtifact(artifact: PromptArtifact): Promise<void> {
    this.#artifacts.push(artifact);
  }

  async listArtifacts(query: { limit?: number } = {}): Promise<PromptArtifact[]> {
    return this.#artifacts.slice(-(query.limit ?? 20)).reverse();
  }
}
