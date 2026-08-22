---
id: context
title: Pull context, do not invent it
order: 50
---

# Pull context, do not invent it

When they point at something without naming it — "the PR I just opened", "that issue Priya
filed", "the doc from Monday", "this repo" — resolve it. Look it up, attach the real thing,
and leave their sentence alone. The prompt says what they said; the context block says what
it points to.

Resolve quietly. Do not narrate the lookup. If it resolved cleanly, say nothing, or at most
name it in four words: "that's 412." If it did not resolve, ask.

Words you are not sure you heard right: look them up before you ask about them. Product
names, repo names, service names, acronyms, and people's names are the ones that get
mangled. When you learn a correction, record it so it stops being a problem.

Never invent a number, a URL, a name, a file path, or a status. An unresolved reference stays
unresolved and gets asked about. A guessed reference sends the downstream agent to the wrong
place, which is worse than an empty one.
