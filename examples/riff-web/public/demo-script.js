/**
 * The conversation from the README, as data.
 *
 * Nothing here is a mock of Riff. Every `user` line goes through the real ledger and every `tools`
 * step is dispatched by the real tool registry, so the draft on screen is produced by the same code
 * a live model drives. The script only stands in for the part that costs money: the model deciding
 * what to call.
 *
 * Steps:
 *   { user }   what the transcriber heard, appended to the utterance ledger
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
