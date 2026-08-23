/**
 * The conversation from the README, as data, with one beat the README leaves out: a subject change
 * partway through, which opens a second prompt and parks the first.
 *
 * Nothing here is a mock of Riff. Every `user` line goes through the real ledger and every `tools`
 * step is dispatched by the real tool registry, so the draft on screen is produced by the same code
 * a live model drives. The script only stands in for the part that costs money: the model deciding
 * what to call.
 *
 * Steps:
 *   { user }   what the transcriber heard, appended to the utterance ledger
 *   { agent }  what Riff says back, spoken in live mode and silent here
 *   { tools }  a turn's worth of tool calls, dispatched together the way a provider delivers them
 */

/** Stands in for whatever id `resolve_reference` hands back, so the script suits any host. */
export const REFERENCE_PLACEHOLDER = "{{reference}}";

export const DEMO_SCRIPT = [
  {
    user: "okay so the export button on the dashboard, it does nothing if you've got more than about a thousand rows. just spins.",
  },

  // The first attempt is a paraphrase, and the grounding check throws it out. Riff normally hides
  // this from the speaker; the demo shows it, because it is the whole argument.
  {
    note: "a paraphrase, which the grounding check rejects",
    tools: [
      {
        name: "draft_update",
        args: {
          operations: [
            { op: "set_title", text: "Fix the export button" },
            {
              op: "upsert_line",
              section: "intent",
              text: "The CSV export functionality fails silently for large result sets",
            },
          ],
        },
      },
    ],
  },

  {
    note: "the same claim, in words they actually said",
    tools: [
      {
        name: "draft_update",
        args: {
          operations: [
            {
              op: "upsert_line",
              section: "intent",
              text: "the export button on the dashboard does nothing if you've got more than about a thousand rows",
            },
            { op: "upsert_line", section: "intent", text: "just spins" },
          ],
        },
      },
    ],
  },

  {
    user: "oh, separate thing, the onboarding doc still says node sixteen. different thing entirely.",
  },

  // The point of the beat. A different request is a different prompt, so the export one is parked
  // rather than overwritten, and both stay open until one of them is sent.
  {
    note: "a different request, so it becomes its own take and the export one is parked",
    tools: [
      { name: "takes", args: { action: "new", label: "onboarding doc" } },
      {
        name: "draft_update",
        args: {
          operations: [
            { op: "set_title", text: "the onboarding doc" },
            {
              op: "upsert_line",
              section: "intent",
              text: "the onboarding doc still says node sixteen",
            },
          ],
        },
      },
    ],
  },

  { agent: "that's a new one. the export thing is parked." },

  { user: "yeah, let's finish the export one first" },

  // `t1` is what the model would have remembered from starting the second take, not something the
  // script knows about the engine: take ids are handed back by every `takes` and `draft_update` call.
  {
    note: "back to the parked take; the onboarding one keeps its line",
    tools: [{ name: "takes", args: { action: "switch", take_id: "t1" } }],
  },

  { user: "and it's related to the PR I just opened I think" },

  {
    note: "looking up what they pointed at instead of asking",
    tools: [
      {
        name: "resolve_reference",
        args: {
          phrase: "the PR I just opened",
          kind: "pull_request",
          recency: "latest",
          actor: "me",
        },
      },
    ],
  },

  { agent: `that's ${REFERENCE_PLACEHOLDER}.` },

  {
    note: "the sentence stays theirs; the resolved PR rides along as context",
    tools: [
      {
        name: "draft_update",
        args: {
          operations: [
            { op: "upsert_line", section: "intent", text: "it's related to the PR I just opened" },
            { op: "attach_context", reference_id: REFERENCE_PLACEHOLDER },
          ],
        },
      },
    ],
  },

  { user: "yeah that one. um, and don't touch the generated files, you know how it is" },

  {
    note: "a standing instruction it has heard before, reattached in their words",
    tools: [{ name: "motifs", args: { action: "attach", motif_id: "m-generated-files" } }],
  },

  {
    user: "I should be able to export like fifty thousand rows without it falling over. send it.",
  },

  {
    tools: [
      {
        name: "draft_update",
        args: {
          operations: [
            {
              op: "upsert_line",
              section: "acceptance",
              text: "I should be able to export like fifty thousand rows without it falling over",
            },
          ],
        },
      },
    ],
  },

  {
    note: "they said to send it, so it goes",
    tools: [{ name: "submit_prompt", args: {} }],
  },
];

/**
 * Fills in the reference placeholder with an id the host actually returned.
 *
 * The script is written against a conversation, not against a particular world, so it cannot know
 * what `resolve_reference` will call the thing it found. With no reference resolved — a repository
 * with no open pull requests, say — the operation that needed one is dropped rather than sent to be
 * rejected, since a missing reference is a fact about the world and not a mistake by the model.
 */
export function substituteReferences(args, referenceId) {
  const operations = args?.operations;
  if (!Array.isArray(operations)) return args;

  const substituted = operations
    .filter((operation) => operation.reference_id !== REFERENCE_PLACEHOLDER || referenceId)
    .map((operation) =>
      operation.reference_id === REFERENCE_PLACEHOLDER
        ? { ...operation, reference_id: referenceId }
        : operation,
    );

  return { ...args, operations: substituted };
}

/**
 * How Riff would say a resolved reference out loud.
 *
 * Nobody reads a URL to someone, and nobody says "hash". A pull request spoken aloud is its number
 * and its subject, which is what the README's "that's 412, chunked uploads" is.
 */
export function spokenReference(candidate) {
  if (!candidate) return null;
  const number = /#(\d+)$/.exec(candidate.identifier ?? "")?.[1];
  const title = candidate.title?.toLowerCase();
  return [number, title].filter(Boolean).join(", ") || candidate.identifier || "that one";
}

/**
 * Puts the spoken form into a scripted line the agent says.
 *
 * With nothing resolved the line becomes a question rather than a claim. Riff does not invent facts:
 * an unresolved reference stays unresolved and gets asked about, so a fallback that asserted "the
 * one you opened most recently" would be the agent making something up on the demo's behalf.
 */
export function substituteSpokenReference(text, spoken) {
  if (!text.includes(REFERENCE_PLACEHOLDER)) return text;
  if (!spoken) return "which one do you mean? I couldn't find it.";
  return text.replaceAll(REFERENCE_PLACEHOLDER, spoken);
}
