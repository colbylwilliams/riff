import type { ContextItem, Line, PromptArtifact, RenderConfig, RenderDisposition, Section } from "./types.ts";
import { SECTIONS } from "./types.ts";
import type { Take } from "./draft.ts";
import type { Lexicon } from "./lexicon.ts";
import { countWords, tidyWhitespace } from "./text.ts";

export interface RenderOptions {
  config: RenderConfig;
  /** Overrides `config.profile` when a host wants a different shape for a specific destination. */
  profile?: string;
}

/** Renders a take as the Markdown the downstream agent receives. */
export function renderPrompt(take: Take, options: RenderOptions): string {
  const name = options.profile ?? options.config.profile;
  const profile = options.config.profiles[name];
  if (!profile) throw new Error(`unknown render profile "${name}"`);

  const blocks: string[] = [];
  const titleDisposition = profile["title"] ?? "omit";
  if (take.title && titleDisposition === "h1") blocks.push(`# ${take.title.text}`);

  for (const section of SECTIONS) {
    const lines = take.linesIn(section);
    if (lines.length === 0) continue;
    const block = renderSection(section, lines, profile[section] ?? "paragraphs", options.config.labels);
    if (block) blocks.push(block);
  }

  const context = take.context();
  if (context.length > 0) {
    const block = renderContext(context, profile["context"] ?? "labeled-list", options.config.labels);
    if (block) blocks.push(block);
  }

  return blocks.join("\n\n").trim();
}

function renderSection(
  section: Section,
  lines: Line[],
  disposition: RenderDisposition,
  labels: Record<string, string>,
): string | null {
  const label = labels[section] ?? titleCase(section);
  const texts = lines.map((line) => line.text);

  switch (disposition) {
    case "omit":
      return null;
    case "paragraphs":
      return texts.map(withTerminalPunctuation).join(" ");
    case "labeled-list":
      return [`**${label}**`, ...texts.map((text) => `- ${text}`)].join("\n");
    case "section-list":
      return [`## ${label}`, "", ...texts.map((text) => `- ${text}`)].join("\n");
    case "h1":
      return `# ${texts.join(" ")}`;
    default:
      return texts.join("\n");
  }
}

function renderContext(
  items: ContextItem[],
  disposition: RenderDisposition,
  labels: Record<string, string>,
): string | null {
  if (disposition === "omit") return null;
  const label = labels["context"] ?? "Context";
  const entries = items.map((item) => `- ${describeContext(item)}`);
  return disposition === "section-list"
    ? [`## ${label}`, "", ...entries].join("\n")
    : [`**${label}**`, ...entries].join("\n");
}

function describeContext(item: ContextItem): string {
  const parts: string[] = [];
  parts.push(item.identifier ?? item.title ?? item.kind);
  if (item.identifier && item.title) parts.push(`"${item.title}"`);
  if (item.state) parts.push(`(${item.state})`);
  if (item.actor) parts.push(`by ${item.actor}`);
  if (item.url) parts.push(`— ${item.url}`);
  if (item.resolvedFrom) parts.push(`— referred to as "${item.resolvedFrom}"`);
  return parts.join(" ");
}

function withTerminalPunctuation(text: string): string {
  const trimmed = tidyWhitespace(text);
  return /[.!?:;]$/.test(trimmed) ? trimmed : `${trimmed}.`;
}

function titleCase(section: string): string {
  return section.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());
}

export interface BuildArtifactOptions extends RenderOptions {
  lexicon: Lexicon;
  utteranceCount: number;
  now?: string;
  provenance?: Partial<PromptArtifact["provenance"]>;
}

/**
 * Turns a take into the artifact that leaves the session.
 *
 * Fidelity is a token-weighted share of the body that is provably the speaker's, so a consumer can
 * tell at a glance whether a prompt was captured or composed, without reading it.
 */
export function buildArtifact(take: Take, options: BuildArtifactOptions): PromptArtifact {
  const now = options.now ?? new Date().toISOString();
  const lines = take.lines();
  const rendered = renderPrompt(take, options);

  let bodyTokens = 0;
  let groundedTokens = 0;
  for (const line of lines) {
    const words = countWords(line.text);
    bodyTokens += words;
    groundedTokens += words * (line.grounding.kind === "derived" ? 0 : line.grounding.ratio);
  }

  const fidelity = bodyTokens === 0 ? 0 : Math.round((groundedTokens / bodyTokens) * 1000) / 1000;
  const terms = options.lexicon.termsUsedIn(lines.map((line) => line.text).join(" "));

  return {
    id: `${take.id}-${now}`,
    takeId: take.id,
    ...(take.label ? { label: take.label } : {}),
    createdAt: take.createdAt,
    updatedAt: take.updatedAt,
    title: take.title ?? { text: fallbackTitle(take), origin: "derived" },
    lines,
    context: take.context(),
    ...(terms.length > 0
      ? {
          terms: terms.map((term) => ({
            canonical: term.canonical,
            kind: term.kind,
            ...(term.heardAs?.length ? { heardAs: term.heardAs } : {}),
            ...(term.definition ? { definition: term.definition } : {}),
          })),
        }
      : {}),
    provenance: {
      fidelity,
      utteranceCount: options.utteranceCount,
      bodyTokens,
      agentAuthoredTokens: Math.round(bodyTokens - groundedTokens),
      ...options.provenance,
    },
    rendered,
    ...(take.target ? { target: take.target } : {}),
    status: take.status,
  };
}

function fallbackTitle(take: Take): string {
  const first = take.linesIn("intent")[0] ?? take.lines()[0];
  if (!first) return "Untitled";
  const words = first.text.split(/\s+/).slice(0, 8).join(" ");
  return words.replace(/[,.;:]$/, "");
}

/**
 * A one or two sentence account of what the draft covers, for when they ask how it is looking.
 * This is the agent's own summary and never becomes part of the prompt.
 */
export function summarizeDraft(take: Take): string {
  const counts = SECTIONS.map((section) => [section, take.linesIn(section).length] as const).filter(
    ([, count]) => count > 0,
  );
  if (counts.length === 0) return "Nothing captured yet.";

  const described = counts
    .map(([section, count]) => `${count} ${section.replace(/_/g, " ")}${count === 1 ? "" : "s"}`)
    .join(", ");
  const context = take.context();
  const attached =
    context.length === 0
      ? ""
      : ` Attached: ${context.map((item) => item.identifier ?? item.title).join(", ")}.`;
  const intent = take.linesIn("intent")[0];

  return `${intent ? `${intent.text} ` : ""}Captured ${described}.${attached}`.trim();
}
