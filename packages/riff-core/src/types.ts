/** Sections of a prompt, ordered by how much they matter to the downstream agent. */
export const SECTIONS = ["intent", "detail", "constraint", "acceptance", "open_question"] as const;

export type Section = (typeof SECTIONS)[number];

export function isSection(value: unknown): value is Section {
  return typeof value === "string" && (SECTIONS as readonly string[]).includes(value);
}

/** How a line in the prompt relates to what the speaker actually said. */
export type GroundingKind = "verbatim" | "trimmed" | "corrected" | "motif" | "derived";

/** How strictly a section is held to the speaker's words. */
export type GroundingMode = "strict" | "motif-or-strict" | "derived";

export interface Utterance {
  id: string;
  /** Exactly what the transcriber produced, before any correction. */
  text: string;
  at: string;
  source: "speech" | "typed";
  /** Transcriber confidence, when the provider reports one. */
  confidence?: number;
}

export interface LexiconTerm {
  canonical: string;
  kind: string;
  /** Ways transcription renders this term incorrectly. */
  heardAs?: string[];
  definition?: string;
  scope?: "session" | "user" | "workspace";
}

export interface GroundingResult {
  ok: boolean;
  /** Share of the candidate's meaningful tokens recoverable from a span of what the speaker said. */
  ratio: number;
  kind: GroundingKind;
  sourceUtteranceIds: string[];
  /** Meaningful candidate tokens with no source. These are the words the agent invented. */
  unmatchedTokens: string[];
}

export interface Line {
  id: string;
  section: Section;
  text: string;
  order: number;
  sourceUtteranceIds: string[];
  motifId?: string;
  supersedes?: string[];
  grounding: {
    ratio: number;
    kind: GroundingKind;
    unmatchedTokens?: string[];
  };
}

export interface ContextItem {
  referenceId: string;
  kind: string;
  title: string;
  identifier?: string;
  url?: string;
  actor?: string;
  timestamp?: string;
  state?: string;
  summary?: string;
  /** The phrase the speaker used, so the prompt and the resolved thing stay connected. */
  resolvedFrom?: string;
  confidence?: number;
}

export interface Motif {
  id: string;
  text: string;
  scope: "user" | "workspace";
  appliesWhen?: string;
  createdAt: string;
  retiredAt?: string;
}

export type TakeStatus = "drafting" | "ready" | "submitted" | "parked" | "discarded";

export interface PromptArtifact {
  id: string;
  takeId: string;
  label?: string;
  createdAt: string;
  updatedAt: string;
  title: { text: string; origin: "derived" | "spoken" };
  lines: Line[];
  context: ContextItem[];
  terms?: Array<{ canonical: string; heardAs?: string[]; definition?: string; kind?: string }>;
  provenance: {
    fidelity: number;
    utteranceCount: number;
    bodyTokens: number;
    agentAuthoredTokens: number;
    agentVersion?: string;
    bundleRevision?: string;
    providerId?: string;
    model?: string;
    sessionId?: string;
    durationMs?: number;
    toolCalls?: Array<{ name: string; at: string; durationMs?: number; ok?: boolean }>;
  };
  rendered: string;
  target?: string;
  status?: TakeStatus;
}

export interface GroundingConfig {
  threshold: number;
  titleThreshold?: number;
  windowSize: number;
  sections: Record<string, GroundingMode>;
  freeTokens: string[];
  filler: string[];
}

export type RenderDisposition = "h1" | "paragraphs" | "labeled-list" | "section-list" | "omit";

export interface RenderConfig {
  profile: string;
  profiles: Record<string, Record<string, RenderDisposition>>;
  labels: Record<string, string>;
  includeProvenanceFooter?: boolean;
}

export interface PolicyConfig {
  maxTakes: number;
  readinessRequires: Section[];
  autoSubmit: boolean;
  readbackDefault: "none" | "gist" | "full";
  persistAudio?: boolean;
  redactSecretsFromTranscript?: boolean;
}

export interface ToolDefinition {
  name: string;
  /** `local` runs inside Riff; `host` is delegated to the embedding application. */
  kind: "local" | "host";
  description: string;
  parameters: Record<string, unknown>;
  returns?: Record<string, unknown>;
  source?: string;
}

export interface InstructionSection {
  id: string;
  title: string;
  order: number;
  source: string;
  text: string;
}

/** Provider-neutral session defaults. Providers map these onto their own wire format. */
export interface SessionDefaults {
  model: { preferred: string; fallbacks?: string[] };
  modalities: { input: string[]; output: string[] };
  voice: { name: string; speed?: number };
  audio: {
    input: {
      encoding: string;
      sampleRate: number;
      channels: number;
      noiseReduction?: string | null;
    };
    output: { encoding: string; sampleRate: number; channels: number };
  };
  turnDetection: {
    mode: "semantic" | "vad" | "manual";
    eagerness?: "low" | "medium" | "high" | "auto";
    autoRespond: boolean;
    allowBargeIn: boolean;
    serverVadFallback?: {
      threshold: number;
      prefixPaddingMs: number;
      silenceDurationMs: number;
    };
  };
  transcription: {
    preferred: string;
    fallbacks?: string[];
    language?: string | null;
    biasing?: { enabled: boolean; maxKeywords: number; promptPreamble: string };
  };
  reasoning?: { effort: "low" | "medium" | "high" };
  limits: { maxOutputTokens: number; maxSessionSeconds: number; toolTimeoutMs: number };
  truncation?: {
    strategy: string;
    retentionRatio: number;
    postInstructionTokenLimit: number;
  };
  toolChoice: string;
  parallelToolCalls?: boolean;
}

export interface AgentBundle {
  id: string;
  name: string;
  version: string;
  revision: string;
  description?: string;
  /** The composed system prompt handed to the realtime model. */
  instructions: string;
  instructionSections: InstructionSection[];
  tools: ToolDefinition[];
  session: SessionDefaults;
  lexicon: { version: number; terms: LexiconTerm[] };
  grounding: GroundingConfig;
  render: RenderConfig;
  policy: PolicyConfig;
}
