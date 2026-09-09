/**
 * The main README's conversation, as data: a riff about offline drafts that draws on a Slack
 * thread, earlier prompts, and a PR, with a tangent kept in its own take.
 *
 * Nothing here is a mock of Riff. Every `user` line goes through the real ledger and every `tools`
 * step is dispatched by the real tool registry, so the draft on screen is produced by the same code
 * a live model drives. The script only stands in for the part that costs money: the model deciding
 * what to call.
 *
 * Steps:
 *   { user }   what the transcriber heard, appended to the utterance ledger
 *   { agent }  a clarification, never appended to the utterance ledger
 *   { tools }  a turn's worth of tool calls, dispatched together the way a provider delivers them
 */

/** Stands in for whatever id `resolve_reference` hands back, so the script suits any host. */
export const REFERENCE_PLACEHOLDER = "{{reference}}";

export const DEMO_SCRIPT = [
  {
    user: "okay, I think we should make drafts work offline. people keep losing edits on the train.",
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
            { op: "set_title", text: "Make drafts work offline" },
            {
              op: "upsert_line",
              section: "intent",
              text: "Implement offline-first persistence and automatic synchronization for draft content",
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
              text: "I think we should make drafts work offline",
            },
            { op: "upsert_line", section: "detail", text: "people keep losing edits on the train" },
          ],
        },
      },
    ],
  },

  { user: "the Slack thread from Monday has the examples" },

  {
    note: "finding the Slack discussion without interrupting the thought",
    tools: [
      { name: "resolve_reference", args: { phrase: "the Slack thread from Monday", kind: "message" } },
    ],
  },

  {
    tools: [
      {
        name: "draft_update",
        args: {
          operations: [
            { op: "upsert_line", section: "detail", text: "the Slack thread from Monday has the examples" },
            { op: "attach_context", reference_id: REFERENCE_PLACEHOLDER },
          ],
        },
      },
    ],
  },

  { user: "didn't I already riff on this last week?" },

  {
    note: "recalling earlier prompts and their recorded outcomes, not copying their words",
    tools: [{ name: "recall_prompts", args: { query: "offline drafts", limit: 2 } }],
  },

  { agent: "The local-drafts session or the sync-retry one?" },

  {
    user: "use the local-drafts session as background, and use the PR I just opened as the starting point. no sync engine yet.",
  },

  {
    tools: [
      { name: "resolve_reference", args: { phrase: "the local-drafts session", kind: "document" } },
    ],
  },

  {
    note: "the earlier session travels as a link, not as new instructions in the speaker's voice",
    tools: [
      {
        name: "draft_update",
        args: {
          operations: [
            { op: "upsert_line", section: "detail", text: "use the local-drafts session as background" },
            { op: "attach_context", reference_id: REFERENCE_PLACEHOLDER },
          ],
        },
      },
    ],
  },

  {
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
    note: "the related PR is attached; the scope still comes from what they just said",
    tools: [
      {
        name: "draft_update",
        args: {
          operations: [
            { op: "upsert_line", section: "detail", text: "use the PR I just opened as the starting point" },
            { op: "upsert_line", section: "constraint", text: "no sync engine yet" },
            { op: "attach_context", reference_id: REFERENCE_PLACEHOLDER },
          ],
        },
      },
    ],
  },

  {
    user: "oh, separate thing, the onboarding doc still says node sixteen. different thing entirely.",
  },

  // A different request is a different prompt, so the offline-drafts one is parked
  // rather than overwritten, and both stay open until one of them is sent. Riff starts the new take
  // without a word about it, which is what `60-corrections` requires.
  {
    note: "a different request, so it becomes its own take and the offline-drafts one is parked",
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

  { user: "let's finish the offline drafts one first" },

  // `t1` is what the model would have remembered from starting the second take, not something the
  // script knows about the engine: take ids are handed back by every `takes` and `draft_update` call.
  {
    note: "back to the parked take; the onboarding one keeps its line",
    tools: [{ name: "takes", args: { action: "switch", take_id: "t1" } }],
  },

  { user: "and don't touch the generated files, you know how it is" },

  {
    note: "a standing instruction it has heard before, reattached in their words",
    tools: [{ name: "motifs", args: { action: "attach", motif_id: "m-generated-files" } }],
  },

  {
    user: "I should be able to close the app offline and come back to my edits. send it.",
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
              text: "I should be able to close the app offline and come back to my edits",
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
