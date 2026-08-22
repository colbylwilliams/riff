/**
 * The demo's server, which does as little as possible.
 *
 * Two jobs. It serves the page and the compiled packages, so the browser can load Riff without a
 * bundler. And it mints the ephemeral client secret for live mode, which is the one part of the
 * provider that has to stay on a server — an API key shipped to a browser is a key you published.
 *
 *   node examples/riff-web/server.mjs
 *   OPENAI_API_KEY=sk-... node examples/riff-web/server.mjs   # enables live mode
 */
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { extname, join, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

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
const bundle = loadBundle(JSON.parse(await readFile(BUNDLE_PATH, "utf8")));

const apiKey = process.env.OPENAI_API_KEY;

const server = createServer(async (request, response) => {
  const url = new URL(request.url ?? "/", `http://${request.headers.host ?? "localhost"}`);

  try {
    if (url.pathname === "/api/riff/config") {
      return json(response, 200, { live: Boolean(apiKey), model: bundle.session.model.preferred });
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
  if (!apiKey) {
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
});
