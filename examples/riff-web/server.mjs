/**
 * The demo's server, which does as little as possible.
 *
 * Three jobs. It serves the page and the compiled packages, so the browser can load Riff without a
 * bundler. It mints the ephemeral client secret for live mode. And it runs the GitHub-backed host on
 * this side of the wire, because a host that can reach GitHub needs a token and a token in a browser
 * is a token you published. Both credentials stay here; the page gets answers, never keys.
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
const { GitHubHost, issueDestination } = await importRiff("@riff/github");
const bundle = loadBundle(JSON.parse(await readFile(BUNDLE_PATH, "utf8")));

const apiKey = process.env.OPENAI_API_KEY;
const githubToken = process.env.GITHUB_TOKEN ?? process.env.GH_TOKEN;
const repository = process.env.GITHUB_REPOSITORY ?? (githubToken ? await detectRepository() : undefined);
const github = Boolean(githubToken && repository);

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
      message: `would have filed an issue in ${repository}; nothing was created`,
    };
  },
};

/** Rebuilt per request so the issue toggle takes effect without restarting anything. */
function githubHost({ allowIssues = false } = {}) {
  // Exactly one default, or the agent is choosing between two things that both claim to be it.
  const destinations = allowIssues
    ? [issueDestination({ repository, default: true })]
    : [{ ...dryRunDestination, default: true }, issueDestination({ repository })];

  return new GitHubHost({ token: githubToken, repository, workspace: repository, destinations });
}

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "localhost"}`);

  try {
    if (url.pathname === "/api/riff/config") {
      return json(response, 200, {
        live: Boolean(apiKey),
        model: bundle.session.model.preferred,
        github: { available: github, repository: repository ?? null },
      });
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

async function mintToken(response) {  if (!apiKey) {
    return json(response, 501, {
      error: "live mode needs OPENAI_API_KEY set on the server; scripted mode needs nothing",
    });
  }

  const maxKeywords = bundle.session.transcription.biasing?.maxKeywords ?? 100;

  try {
    const secret = await mintClientSecret({
      apiKey,
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

/** The RiffHost surface, and the only methods this endpoint will dispatch. */
const HOST_METHODS = {
  environment: (host) => host.environment(),
  resolveReference: (host, body) => host.resolveReference(body.request ?? {}),
  lookupTerm: (host, body) => host.lookupTerm(body.request ?? {}),
  recallPrompts: (host, body) => host.recallPrompts(body.request ?? {}),
  submitPrompt: (host, body) => host.submitPrompt(body.artifact, body.options ?? {}),
};

async function callHost(request, response) {
  if (!github) {
    return json(response, 501, {
      error: "no GitHub host configured; set GITHUB_TOKEN (and GITHUB_REPOSITORY if it cannot be detected)",
    });
  }

  const body = JSON.parse(await readBody(request));
  const method = HOST_METHODS[body.method];
  if (!method) return json(response, 400, { error: `no host method "${body.method}"` });

  try {
    json(response, 200, (await method(githubHost(body.settings ?? {}), body)) ?? {});
  } catch (error) {
    // Riff turns this into a tool error the agent can talk about, which beats a dead session.
    json(response, 502, { error: error.message });
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

server.listen(port, () => {
  console.log(`riff demo   http://localhost:${port}`);
  console.log(
    apiKey
      ? `live mode   enabled (${bundle.session.model.preferred})`
      : "live mode   disabled — set OPENAI_API_KEY to enable it; scripted mode works without one",
  );
  console.log(
    github
      ? `host        GitHub, resolving against ${repository}`
      : "host        demo (a canned PR and motif) — set GITHUB_TOKEN to resolve against a real repo",
  );
});
