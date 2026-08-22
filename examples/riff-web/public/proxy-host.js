/**
 * A RiffHost that runs somewhere else.
 *
 * A host is whatever the embedding application knows about the world, and it runs wherever the
 * session runs. In this demo the session runs in the page, so a GitHub-backed host would put a
 * GitHub token in the page — the same mistake as shipping an API key to a browser.
 *
 * So the host seam is taken at its word and moved across the wire. Each of the five methods becomes
 * one POST, the server holds the token and delegates to the real `GitHubHost`, and Riff cannot tell
 * the difference. That is worth seeing on its own: the interface is small enough to be remoted
 * without ceremony, which is how you would deploy this for real.
 */
export class ProxyHost {
  #endpoint;
  #settings;

  /**
   * @param {object} [options]
   * @param {string} [options.endpoint]
   * @param {() => object} [options.settings]  read at call time, so a toggle takes effect at once
   */
  constructor({ endpoint = "/api/riff/host", settings = () => ({}) } = {}) {
    this.#endpoint = endpoint;
    this.#settings = settings;
  }

  environment() {
    return this.#call("environment", {});
  }

  resolveReference(request) {
    return this.#call("resolveReference", { request: strip(request) }, request.signal);
  }

  lookupTerm(request) {
    return this.#call("lookupTerm", { request: strip(request) }, request.signal);
  }

  recallPrompts(request) {
    return this.#call("recallPrompts", { request: strip(request) }, request.signal);
  }

  submitPrompt(artifact, options = {}) {
    return this.#call("submitPrompt", { artifact, options: strip(options) }, options.signal);
  }

  async #call(method, payload, signal) {
    const response = await fetch(this.#endpoint, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ method, ...payload, settings: this.#settings() }),
      // Riff aborts a host call it has stopped waiting for. Passing the signal through means the
      // request is actually cancelled rather than left running against GitHub.
      ...(signal ? { signal } : {}),
    });

    if (!response.ok) {
      const detail = await response.text().catch(() => "");
      throw new Error(`host.${method} failed (${response.status}): ${detail.slice(0, 200)}`);
    }
    return response.json();
  }
}

/** Drops the AbortSignal, which is honored by the fetch rather than serialized into it. */
function strip(request = {}) {
  const { signal: _signal, ...rest } = request;
  return rest;
}
