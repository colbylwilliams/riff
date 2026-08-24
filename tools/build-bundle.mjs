import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

/**
 * Compiles the language-neutral agent definition in core/agent into a single bundle that every
 * platform binding loads. The compiled bundle is committed so Swift, Rust, and Kotlin builds do not
 * need a Node toolchain; `--check` fails when the committed copy has drifted from source.
 */

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, "..");
const agentDir = join(root, "core", "agent");
const outputs = [
  join(root, "core", "dist", "riff-agent.bundle.json"),
  join(root, "swift", "Sources", "RiffCore", "Resources", "riff-agent.bundle.json"),
  join(root, "rust", "riff-core", "resources", "riff-agent.bundle.json"),
];

const conformanceSource = join(root, "core", "conformance", "cases");
const conformanceTargets = [
  join(root, "swift", "Tests", "RiffCoreTests", "Resources"),
  join(root, "rust", "riff-core", "tests", "resources"),
];

const check = process.argv.includes("--check");

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));

function parseFrontMatter(raw, path) {
  const match = /^---\n([\s\S]*?)\n---\n?/.exec(raw);
  if (!match) throw new Error(`${path}: instruction file is missing front matter`);
  const meta = {};
  for (const line of match[1].split("\n")) {
    const at = line.indexOf(":");
    if (at === -1) throw new Error(`${path}: malformed front matter line "${line}"`);
    meta[line.slice(0, at).trim()] = line.slice(at + 1).trim();
  }
  for (const key of ["id", "title", "order"]) {
    if (!(key in meta)) throw new Error(`${path}: front matter is missing "${key}"`);
  }
  return { meta, body: raw.slice(match[0].length).trim() };
}

const manifest = readJson(join(agentDir, "agent.json"));

const listed = new Set([...manifest.instructions, ...manifest.tools]);
for (const dir of ["instructions", "tools"]) {
  for (const name of readdirSync(join(agentDir, dir))) {
    const rel = `${dir}/${name}`;
    if (!listed.has(rel)) throw new Error(`${rel} exists but is not listed in agent.json`);
  }
}

const sections = manifest.instructions.map((rel) => {
  const { meta, body } = parseFrontMatter(readFileSync(join(agentDir, rel), "utf8"), rel);
  return { id: meta.id, title: meta.title, order: Number(meta.order), source: rel, text: body };
});

const orders = sections.map((s) => s.order);
if (new Set(orders).size !== orders.length) throw new Error("instruction sections have duplicate order values");
if (orders.some((o, i) => i > 0 && o <= orders[i - 1])) {
  throw new Error("instruction sections are not listed in ascending order");
}

const seenTool = new Set();
const tools = manifest.tools.map((rel) => {
  const tool = readJson(join(agentDir, rel));
  for (const key of ["name", "kind", "description", "parameters"]) {
    if (!(key in tool)) throw new Error(`${rel}: tool is missing "${key}"`);
  }
  if (seenTool.has(tool.name)) throw new Error(`${rel}: duplicate tool name "${tool.name}"`);
  seenTool.add(tool.name);
  if (tool.parameters.type !== "object") throw new Error(`${rel}: parameters must be an object schema`);
  return { ...tool, source: rel };
});

const toolNames = new Set(tools.map((t) => t.name));
const toolsSection = sections.find((s) => s.id === "tools");
if (toolsSection) {
  for (const name of toolNames) {
    if (!toolsSection.text.includes(`\`${name}\``)) {
      throw new Error(`tool "${name}" is never mentioned in the tools instruction section`);
    }
  }
}

for (const section of Object.keys(manifest.grounding.sections)) {
  const known = ["intent", "detail", "constraint", "acceptance", "open_question"];
  if (!known.includes(section)) throw new Error(`grounding.sections has unknown section "${section}"`);
}

const profile = manifest.render.profiles[manifest.render.profile];
if (!profile) throw new Error(`render.profile "${manifest.render.profile}" is not defined in render.profiles`);

const instructions = sections.map((s) => s.text).join("\n\n");

const bundle = {
  id: manifest.id,
  name: manifest.name,
  version: manifest.version,
  description: manifest.description,
  instructions,
  instructionSections: sections,
  tools,
  session: readJson(join(agentDir, manifest.session)),
  lexicon: manifest.lexicon ? readJson(join(agentDir, manifest.lexicon)) : { version: 1, terms: [] },
  grounding: manifest.grounding,
  render: manifest.render,
  policy: manifest.policy,
};

if (bundle.policy.autoSubmit !== false) {
  throw new Error("policy.autoSubmit must be false: the speaker decides when a prompt is sent");
}

bundle.revision = createHash("sha256").update(JSON.stringify(bundle)).digest("hex").slice(0, 16);

const serialized = `${JSON.stringify(bundle, null, 2)}\n`;
let drifted = false;

for (const out of outputs) {
  const current = existsSync(out) ? readFileSync(out, "utf8") : null;
  if (current === serialized) continue;
  drifted = true;
  if (!check) {
    mkdirSync(dirname(out), { recursive: true });
    writeFileSync(out, serialized);
  }
}

const approxTokens = Math.round(instructions.length / 4);

// Bindings that cannot run Node still have to prove they satisfy the conformance suite, so the
// cases are mirrored into each binding's test target the same way the bundle is.
for (const name of readdirSync(conformanceSource)) {
  if (!name.endsWith(".json")) continue;
  const body = readFileSync(join(conformanceSource, name), "utf8");
  for (const conformanceTarget of conformanceTargets) {
    const target = join(conformanceTarget, name);
    if (existsSync(target) && readFileSync(target, "utf8") === body) continue;
    drifted = true;
    if (!check) {
      mkdirSync(conformanceTarget, { recursive: true });
      writeFileSync(target, body);
    }
  }
}

if (check && drifted) {
  console.error("agent bundle is out of date; run `npm run bundle` and commit the result");
  process.exit(1);
}

console.log(
  `riff-agent ${bundle.version} rev ${bundle.revision} — ` +
    `${sections.length} instruction sections (~${approxTokens} tokens), ${tools.length} tools, ` +
    `${bundle.lexicon.terms.length} seed terms${check ? " (up to date)" : ""}`,
);
