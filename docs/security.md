# Security and privacy

Riff listens continuously, holds a verbatim record of what was said, and reaches into systems on the
speaker's behalf. Each of those deserves a stated position.

## Credentials

**No long-lived credential reaches a client.** Devices connect with ephemeral client secrets minted
by your backend, behind your own authentication. `mintClientSecret` is the only part of the OpenAI
provider that must run server-side, and it exists so that nothing else has to.

An API key shipped in an app bundle is a key you have published. The `apiKey` credential mode exists
for servers and tests, and is named to make that obvious.

Secrets are cached until shortly before expiry, so reconnecting does not make an unnecessary round
trip, and expiry is honored rather than assumed.

Pass a **hashed** user identifier as `safetyIdentifier`, never a raw one. It exists for abuse
monitoring, not identification.

## What is kept, and what is not

**Audio is not persisted.** `policy.persistAudio` is false. Riff keeps transcripts, because the
prompt is built from them; it does not keep the recording. Applications that want recordings must
implement that themselves, and should tell people.

**Transcripts are the sensitive artifact.** The utterance ledger holds everything said during a
session, including the tangents that never made it into a prompt. It lives in memory for the
lifetime of the session and is not written by the engine. What a host's `RiffStore` chooses to
persist — learned terms, motifs, submitted artifacts — is that host's decision, and worth being
deliberate about.

**Submitted artifacts contain the body verbatim.** If prompts are submitted somewhere durable, that
destination now holds the speaker's words. This is usually the point, but it should be a choice.

## Redaction

Credentials that arrive through typed input are stripped before anything is stored. Redaction runs
on entry to the ledger, so a leaked token never reaches the ledger, the draft, the artifact, or the
model.

Covered: GitHub tokens and fine-grained PATs, OpenAI keys and ephemeral secrets, AWS access key ids,
Slack tokens, and bearer or `api_key` values.

This is a backstop, not a guarantee. It is pattern-based, so it will not catch a novel credential
format or a password read aloud. Speech is unlikely to produce a well-formed token, which is why the
patterns target the typed path.

Controlled by `policy.redactSecretsFromTranscript`, on by default.

## What Riff will not do

**It does not crawl.** References are resolved through the host. A link that is not a recognized
resource is recorded as a URL and not fetched. Riff will not pull arbitrary pages into the model's
context because someone said a domain name.

**It does not invent facts.** No fabricated PR numbers, URLs, names, file paths, or statuses. An
unresolved reference stays unresolved and gets asked about. This is a security property as much as a
quality one: a plausible wrong identifier sends the downstream agent somewhere real.

**It does not send without being told.** `policy.autoSubmit` must be false, and both `loadBundle`
implementations reject a bundle where it is not. There is no configuration that makes Riff submit on
its own.

## Host permissions

A host runs with whatever credentials you give it, and its tools are callable by the model on the
speaker's behalf. Scope narrowly.

The shipped GitHub host reads by default and writes nothing unless a destination is configured — no
issue is opened, nothing is commented on. Adding `issueDestination` is an explicit choice to grant
write access, and destinations are named so a speaker can direct where something goes.

When implementing a host, treat every method as reachable by a model interpreting speech in a noisy
room. Read operations should be scoped to what the speaker can already see. Write operations should
be limited to `submitPrompt` and should be idempotent where they can be.

## Model and provider considerations

**Instructions can be truncated.** A long realtime session drops old context to stay within the
window, and instructions are context. `truncation.token_limits.post_instructions` protects them — an
agent that quietly loses its rules mid-conversation is a real risk, not a theoretical one.

**Sessions have a maximum duration.** Riff emits `expiring` events at five minutes and one minute
remaining so an application can warn or hand off, rather than having the connection disappear
mid-sentence.

**Providers see everything said.** Choosing a provider is choosing who hears the conversation.
The provider interface is drawn where it is partly so an on-device implementation is a substitution
rather than a rewrite.

## Reporting

Report suspected vulnerabilities privately to the repository maintainers rather than in a public
issue.
