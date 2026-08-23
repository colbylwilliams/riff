/**
 * Wraps any provider so the demo can watch tool results go back to the model.
 *
 * Riff's own event stream reports that a tool ran, not what it returned, which is the right call for
 * an embedding application — a UI has no business reading the model's mail. But the most interesting
 * thing this project does happens inside one of those results: `draft_update` rejecting a paraphrase
 * and naming the words the model invented. A demo that cannot show that is not showing the argument.
 *
 * Doing it as a decorator keeps the engine untouched and works for the scripted provider and the
 * live one alike, which is a fair advertisement for how small the provider seam is.
 */
export function observeProvider(provider, { onToolResult } = {}) {
  return {
    id: provider.id,
    get capabilities() {
      return provider.capabilities;
    },
    biasingStyle: provider.biasingStyle?.bind(provider),

    async connect(request) {
      const connection = await provider.connect(request);
      const pending = new Map();

      connection.on((event) => {
        if (event.type !== "tool.calls") return;
        for (const call of event.calls) pending.set(call.callId, call);
      });

      return {
        ...connection,
        // Spread drops the prototype's getters, so the readonly fields are restated explicitly.
        sessionId: connection.sessionId,
        model: connection.model,
        sendAudio: (chunk) => connection.sendAudio(chunk),
        commitAudio: () => connection.commitAudio(),
        sendText: (text, options) => connection.sendText(text, options),
        requestResponse: () => connection.requestResponse(),
        cancelResponse: () => connection.cancelResponse(),
        updateSession: (patch) => connection.updateSession(patch),
        on: (listener) => connection.on(listener),
        close: (reason) => connection.close(reason),

        respondToTool(callId, resultJson) {
          const call = pending.get(callId);
          pending.delete(callId);
          try {
            onToolResult?.({
              name: call?.name ?? "unknown",
              args: call ? safeParse(call.argumentsJson) : {},
              result: safeParse(resultJson),
            });
          } catch {
            // Observation must never break the conversation.
          }
          connection.respondToTool(callId, resultJson);
        },
      };
    },
  };
}

function safeParse(json) {
  try {
    return JSON.parse(json);
  } catch {
    return {};
  }
}
