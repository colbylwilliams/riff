import { MemoryStore, NullHost } from "@riff/core";

import { DEMO_MOTIFS, DemoHost } from "./demo-host.js";
import { ProxyHost } from "./proxy-host.js";

export function createScriptedContext() {
  return {
    host: new DemoHost(),
    store: new MemoryStore({ motifs: DEMO_MOTIFS }),
  };
}

export function createLiveContext({ githubAvailable = false, settings } = {}) {
  return {
    host: githubAvailable ? new ProxyHost({ settings }) : new NullHost(),
    store: new MemoryStore(),
  };
}
