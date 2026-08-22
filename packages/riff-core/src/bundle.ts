import type { AgentBundle, SessionDefaults } from "./types.ts";
import { SECTIONS } from "./types.ts";

/**
 * Loads the compiled agent bundle and refuses anything that would let the agent drift from its
 * contract. The bundle is produced by tools/build-bundle.mjs and shipped to every platform binding,
 * so these checks are the last line of defense against a binding running a modified definition.
 */
export function loadBundle(input: unknown): AgentBundle {
  if (typeof input !== "object" || input === null) throw new Error("agent bundle must be an object");
  const bundle = input as Partial<AgentBundle>;

  for (const key of ["id", "version", "revision", "instructions"] as const) {
    if (typeof bundle[key] !== "string" || bundle[key].length === 0) {
      throw new Error(`agent bundle is missing "${key}"`);
    }
  }

  if (!Array.isArray(bundle.tools) || bundle.tools.length === 0) {
    throw new Error("agent bundle has no tools");
  }
  for (const tool of bundle.tools) {
    if (!tool.name || !tool.description || !tool.parameters) {
      throw new Error(`tool "${tool?.name ?? "(unnamed)"}" is incomplete`);
    }
    if (tool.kind !== "local" && tool.kind !== "host") {
      throw new Error(`tool "${tool.name}" has unknown kind "${tool.kind}"`);
    }
  }

  const grounding = bundle.grounding;
  if (!grounding) throw new Error("agent bundle is missing grounding configuration");
  if (!(grounding.threshold > 0 && grounding.threshold <= 1)) {
    throw new Error("grounding.threshold must be greater than 0 and at most 1");
  }
  if (!(grounding.windowSize >= 1)) throw new Error("grounding.windowSize must be at least 1");
  for (const section of Object.keys(grounding.sections)) {
    if (!(SECTIONS as readonly string[]).includes(section)) {
      throw new Error(`grounding.sections has unknown section "${section}"`);
    }
  }

  const render = bundle.render;
  if (!render?.profiles?.[render.profile]) {
    throw new Error(`render profile "${render?.profile}" is not defined`);
  }

  const policy = bundle.policy;
  if (!policy) throw new Error("agent bundle is missing policy configuration");
  if (policy.autoSubmit !== false) {
    throw new Error("policy.autoSubmit must be false: the speaker decides when a prompt is sent");
  }
  if (!Array.isArray(policy.readinessRequires)) throw new Error("policy.readinessRequires must be an array");

  if (!bundle.session) throw new Error("agent bundle is missing session defaults");
  if (!bundle.lexicon) bundle.lexicon = { version: 1, terms: [] };
  if (!Array.isArray(bundle.instructionSections)) bundle.instructionSections = [];

  return bundle as AgentBundle;
}

export interface SessionOverrides {
  model?: string;
  voice?: string;
  speed?: number;
  language?: string | null;
  transcriptionModel?: string;
  turnDetection?: Partial<SessionDefaults["turnDetection"]>;
  maxOutputTokens?: number;
  reasoningEffort?: "low" | "medium" | "high";
}

/** Applies host overrides to the bundle's session defaults without mutating the bundle. */
export function withOverrides(defaults: SessionDefaults, overrides: SessionOverrides = {}): SessionDefaults {
  return {
    ...defaults,
    model: { ...defaults.model, ...(overrides.model ? { preferred: overrides.model } : {}) },
    voice: {
      ...defaults.voice,
      ...(overrides.voice ? { name: overrides.voice } : {}),
      ...(overrides.speed === undefined ? {} : { speed: overrides.speed }),
    },
    turnDetection: { ...defaults.turnDetection, ...overrides.turnDetection },
    transcription: {
      ...defaults.transcription,
      ...(overrides.transcriptionModel ? { preferred: overrides.transcriptionModel } : {}),
      ...(overrides.language === undefined ? {} : { language: overrides.language }),
    },
    ...(overrides.reasoningEffort ? { reasoning: { effort: overrides.reasoningEffort } } : {}),
    limits: {
      ...defaults.limits,
      ...(overrides.maxOutputTokens === undefined ? {} : { maxOutputTokens: overrides.maxOutputTokens }),
    },
  };
}
