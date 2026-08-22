/**
 * The demo's server, which does as little as possible.
 *
 * Three jobs. It serves the page and the compiled packages, so the browser can load Riff without a
 * bundler. It mints the ephemeral client secret for live mode. And it runs the GitHub-backed host on
 * this side of the wire, because a host that can reach GitHub needs a token and a token in a browser
 * is a token you published. Both credentials stay here; the page gets answers, never keys.
 *
 * Credentials can arrive from the environment or be pasted into the page, which posts them here once
 * and never holds them. Either way they live in this process's memory, are never written to disk,
 * and go no further than the API they authenticate.
 *
 *   node examples/riff-web/server.mjs
 *   OPENAI_API_KEY=sk-... node examples/riff-web/server.mjs           # enables live mode
 *   GITHUB_TOKEN=$(gh auth token) node examples/riff-web/server.mjs   # resolves against a real repo
 */
import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { extname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const here = fileURLToPath(new URL(".", import.meta.url));
const repoRoot = resolve(here, "../..");
const port = Number(process.env.PORT ?? 4173);
const address = loopbackOnly(process.env.HOST ?? "127.0.0.1");

/**
 * This process holds an OpenAI key and a GitHub token, and it accepts requests with no `Origin` so
 * that curl still works. Those two together mean binding anywhere but loopback would let anything
 * that can reach the port mint secrets and spend the token, so a wider bind is refused outright
 * rather than honored with a warning.
 */
function loopbackOnly(host) {
  if (["127.0.0.1", "localhost", "::1", "[::1]"].includes(host)) return host;
  console.error(`refusing to bind to ${host}: this server holds credentials and is loopback only`);
  process.exit(1);
}

/** URL prefixes mapped onto directories. First match wins, so `/` is last. */
const MOUNTS = [
  { prefix: "/vendor/riff-core/", dir: join(repoRoot, "packages/riff-core/dist") },
  { prefix: "/vendor/riff-openai-realtime/", dir: join(repoRoot, "packages/riff-openai-realtime/dist") },
  { prefix: "/", dir: join(here, "public") },
];

const BUNDLE_PATH = join(repoRoot, "core/dist/riff-agent.bundle.json");

const CONTENT_TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
};

const { loadBundle } = await importRiff("@riff/core");
const { mintClientSecret } = await importRiff("@riff/openai-realtime");
const { GitHubClient, GitHubHost, issueDestination } = await importRiff("@riff/github");
const bundle = loadBundle(JSON.parse(await readFile(BUNDLE_PATH, "utf8")));

/**
 * Everything secret this process knows, and where it came from.
 *
 * `source` is reported to the page so someone can tell a key they exported in a shell from one they
 * pasted a moment ago. The values themselves are never reported, only whether they are there.
 */
const credentials = {
  openai: { token: process.env.OPENAI_API_KEY, source: process.env.OPENAI_API_KEY ? "environment" : null },
  github: {
    token: process.env.GITHUB_TOKEN ?? process.env.GH_TOKEN,
    source: process.env.GITHUB_TOKEN ?? process.env.GH_TOKEN ? "environment" : null,
    login: null,
  },
  repository: process.env.GITHUB_REPOSITORY,
};

if (credentials.github.token && !credentials.repository) {
  credentials.repository = await detectRepository();
}

const githubReady = () => Boolean(credentials.github.token && credentials.repository);

function describeConfig() {
  return {
    live: Boolean(credentials.openai.token),
    model: bundle.session.model.preferred,
    openai: { source: credentials.openai.source },
    github: {
      available: githubReady(),
      repository: credentials.repository ?? null,
      source: credentials.github.source,
      login: credentials.github.login,
    },
  };
}

/**
 * A destination that renders and reports but does not deliver.
 *
 * Replaying the script ends in `submit_prompt`, so without this every playthrough would file an
 * issue. Filing for real stays one checkbox away rather than one click away by accident.
 */
const dryRunDestination = {
  id: "dry-run",
  label: "Dry run (nothing is created)",
  async send(artifact) {
    return {
      submitted: true,
      promptId: artifact.id,
      destination: "dry-run",
      message: `would have filed an issue in ${credentials.repository}; nothing was created`,
    };
  },
};

/** Rebuilt per request so the issue toggle takes effect without restarting anything. */
function githubHost({ allowIssues = false } = {}) {
  const repository = credentials.repository;
  // The real destination is not merely non-default when issues are off — it is absent. Offering it
  // at all would let the model honor "file it as an issue" while the page promises a dry run.
  const destinations = allowIssues
    ? [issueDestination({ repository, default: true })]
    : [{ ...dryRunDestination, default: true }];

  return new GitHubHost({
    token: credentials.github.token,
    repository,
    workspace: repository,
    destinations,
  });
}

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "localhost"}`);

  try {
    if (url.pathname.startsWith("/api/")) {
      // A page on another origin can send a POST here even though it cannot read the reply, which
      // would be enough to spend a stored token. Same-origin requests carry a matching Origin;
      // curl and friends send none at all.
      const origin = request.headers.origin;
      if (origin && origin !== `http://${request.headers.host}`) {
        return json(response, 403, { error: "cross-origin requests are not accepted" });
      }
    }

    if (url.pathname === "/api/riff/config") return json(response, 200, describeConfig());

    if (url.pathname === "/api/riff/credentials") {
      if (request.method !== "POST") return json(response, 405, { error: "use POST" });
      return await setCredentials(request, response);
    }

    if (url.pathname === "/api/riff/host") {
      if (request.method !== "POST") return json(response, 405, { error: "use POST" });
      return await callHost(request, response);
    }

    if (url.pathname === "/api/riff/token") {
      if (request.method !== "POST") return json(response, 405, { error: "use POST" });
      return await mintToken(response);
    }

    if (url.pathname === "/riff-agent.bundle.json") {
      return send(response, 200, ".json", await readFile(BUNDLE_PATH));
    }

    return await serveStatic(url.pathname, response);
  } catch (error) {
    json(response, 500, { error: error.message });
  }
});

async function mintToken(response) {
  if (!credentials.openai.token) {
    return json(response, 501, {
      error: "live mode needs an OpenAI API key; set OPENAI_API_KEY or paste one into the page",
    });
  }

  const maxKeywords = bundle.session.transcription.biasing?.maxKeywords ?? 100;

  try {
    const secret = await mintClientSecret({
      apiKey: credentials.openai.token,
      session: bundle.session,
      instructions: bundle.instructions,
      tools: bundle.tools,
      vocabulary: bundle.lexicon.terms.map((term) => term.canonical).slice(0, maxKeywords),
      expiresInSeconds: 600,
    });
    // Only the ephemeral secret crosses this line. The API key never does.
    json(response, 200, { value: secret.value, expiresAt: secret.expiresAt });
  } catch (error) {
    json(response, 502, { error: error.message });
  }
}

/**
 * Takes credentials pasted into the page and keeps them in memory.
 *
 * Each one is checked against the API it is for before being kept, so a mistyped key fails here,
 * next to the field it was typed into, instead of surfacing later as a failed connection or an
 * agent that cannot resolve anything. Nothing is written to disk and nothing is echoed back.
 */
async function setCredentials(request, response) {
  const body = JSON.parse(await readBody(request));
  const problems = [];

  if ("openaiApiKey" in body) {
    const token = trimmed(body.openaiApiKey);
    if (!token) {
      credentials.openai = { token: undefined, source: null };
    } else {
      try {
        await mintClientSecret({
          apiKey: token,
          session: bundle.session,
          instructions: bundle.instructions,
          tools: bundle.tools,
          expiresInSeconds: 60,
        });
        credentials.openai = { token, source: "pasted" };
      } catch (error) {
        problems.push(`OpenAI key rejected: ${tidy(error.message)}`);
      }
    }
  }

  if ("githubToken" in body) {
    const token = trimmed(body.githubToken);
    if (!token) {
      credentials.github = { token: undefined, source: null, login: null };
    } else {
      try {
        const user = await new GitHubClient({ token }).get("/user");
        credentials.github = { token, source: "pasted", login: user.login };
      } catch (error) {
        problems.push(`GitHub token rejected: ${tidy(error.message)}`);
      }
    }
  }

  if ("repository" in body) {
    credentials.repository = trimmed(body.repository) ?? undefined;
  }

  // A token with nowhere to point is not usable, so fall back to the checkout's own remote.
  if (credentials.github.token && !credentials.repository) {
    credentials.repository = await detectRepository();
    if (!credentials.repository) problems.push("could not work out a repository; enter owner/name");
  }

  json(response, problems.length > 0 ? 400 : 200, { ...describeConfig(), problems });
}

function trimmed(value) {
  return typeof value === "string" && value.trim().length > 0 ? value.trim() : undefined;
}

/** These errors carry a raw JSON body, which reads badly under a text field. */
function tidy(message) {
  const flattened = String(message).replace(/\s+/g, " ").trim();
  const detail = /"message":\s*"([^"]+)"/.exec(flattened)?.[1];
  const status = /\((\d{3})\)/.exec(flattened)?.[1];
  if (detail) return status ? `${detail} (${status})` : detail;
  return flattened.length > 160 ? `${flattened.slice(0, 157)}…` : flattened;
}

/**
 * The RiffHost surface, and the only methods this endpoint will dispatch.
 *
 * Each takes the signal so an abandoned request actually stops the outbound GitHub call. Riff aborts
 * a host call it has stopped waiting for, and `submitPrompt` has a side effect the speaker can see —
 * without this, a timed-out submission finishes anyway and the retry files a second issue.
 */
const HOST_METHODS = {
  environment: (host) => host.environment(),
  resolveReference: (host, body, signal) => host.resolveReference({ ...body.request, signal }),
  lookupTerm: (host, body, signal) => host.lookupTerm({ ...body.request, signal }),
  recallPrompts: (host, body, signal) => host.recallPrompts({ ...body.request, signal }),
  submitPrompt: (host, body, signal) => host.submitPrompt(body.artifact, { ...body.options, signal }),
};

async function callHost(request, response) {
  if (!githubReady()) {
    return json(response, 501, {
      error: "no GitHub host configured; paste a token into the page or set GITHUB_TOKEN",
    });
  }

  const body = JSON.parse(await readBody(request));
  const method = HOST_METHODS[body.method];
  if (!method) return json(response, 400, { error: `no host method "${body.method}"` });

  // The browser aborts its fetch when Riff gives up, which closes this socket; that is the only
  // signal available here that nobody is waiting for the answer any more.
  const controller = new AbortController();
  const abort = () => controller.abort();
  request.once("aborted", abort);
  response.once("close", abort);

  try {
    json(response, 200, (await method(githubHost(body.settings ?? {}), body, controller.signal)) ?? {});
  } catch (error) {
    // Riff turns this into a tool error the agent can talk about, which beats a dead session.
    if (!response.writableEnded) json(response, 502, { error: error.message });
  } finally {
    request.off("aborted", abort);
    response.off("close", abort);
  }
}

function readBody(request) {
  return new Promise((resolve, reject) => {
    let body = "";
    request.setEncoding("utf8");
    request.on("data", (chunk) => {
      body += chunk;
      // A prompt artifact is the largest thing that arrives here, and it is text.
      if (body.length > 1_000_000) reject(new Error("request body too large"));
    });
    request.on("end", () => resolve(body));
    request.on("error", reject);
  });
}

/** Works out which repository to resolve against, so the usual case needs no configuration. */
async function detectRepository() {
  try {
    const { stdout } = await promisify(execFile)("git", ["remote", "get-url", "origin"], { cwd: repoRoot });
    const match = /github\.com[:/]([\w.-]+\/[\w.-]+?)(?:\.git)?\s*$/.exec(stdout);
    return match?.[1];
  } catch {
    return undefined;
  }
}

async function serveStatic(pathname, response) {
  const requested = pathname === "/" ? "/index.html" : pathname;

  for (const mount of MOUNTS) {
    if (!requested.startsWith(mount.prefix)) continue;

    const relative = requested.slice(mount.prefix.length);
    const target = resolve(mount.dir, relative);
    // Keeps a crafted path such as /vendor/riff-core/../../../.env inside the mount.
    if (target !== mount.dir && !target.startsWith(mount.dir + sep)) continue;

    try {
      return send(response, 200, extname(target), await readFile(target));
    } catch {
      continue;
    }
  }

  json(response, 404, { error: `nothing at ${pathname}` });
}

function send(response, status, extension, body) {
  response.writeHead(status, {
    "Content-Type": CONTENT_TYPES[extension] ?? "application/octet-stream",
    "Cache-Control": "no-store",
  });
  response.end(body);
}

function json(response, status, body) {
  send(response, status, ".json", JSON.stringify(body));
}

/** Turns the usual "did you build?" stack trace into an instruction. */
async function importRiff(specifier) {
  try {
    return await import(specifier);
  } catch (error) {
    console.error(`\ncould not load ${specifier}. Run \`npm install && npm run build\` first.\n`);
    throw error;
  }
}

server.listen(port, address, () => {
  console.log(`riff demo   http://localhost:${port}`);
  console.log(
    credentials.openai.token
      ? `live mode   enabled (${bundle.session.model.preferred})`
      : "live mode   disabled — paste an OpenAI key into the page, or set OPENAI_API_KEY",
  );
  console.log(
    githubReady()
      ? `host        GitHub, resolving against ${credentials.repository}`
      : "host        demo (a canned PR and motif) — paste a GitHub token into the page to use a real repo",
  );
});
