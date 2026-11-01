---
name: zipper-docs
description: Place, write and check Zipper documentation. Use when adding or changing any Markdown page, writing an experiment record, moving or merging pages, or updating docs/results.md.
---

# Zipper docs

Every page has one job and one place. `cargo xtask lint-docs` checks the mechanical rules. This skill covers where a page goes and how it reads.

## Put the page in its place

| Content | Place |
| --- | --- |
| What Zipper is and a map of the docs | `README.md`, kept short |
| Steps to do a task | `docs/usage.md` |
| Why the design is the way it is | `docs/design.md` |
| Open questions and planned experiments | `docs/research.md` |
| Milestones and their exit checks | `docs/roadmap.md` |
| Current headline numbers | `docs/results.md`, copied from records |
| Formats, limits and options | `docs/reference/` |
| One experiment and its raw data | `benchmarks/<area>/<experiment>/README.md` |
| The list of experiments | `benchmarks/README.md` |
| Rules for agents | `AGENTS.md` |

Before adding a page, look for the page that already owns the topic. Update it instead. Link every new page from a page that `README.md` reaches.

## Write an experiment record

Use exactly six level-2 headings, in this order. They are `## Question`, `## Setup`, `## Results`, `## Decision`, `## Limits` and `## Reproduce`. Under the title, give the verdict in one or two sentences, including its most important caveat. If a section has no information, write one honest sentence saying so.

## Write so a tired engineer understands on the first read

- One purpose per page. Do not mix steps, reference and argument.
- Headings in sentence case that state the point.
- One thought per sentence. Active voice. The real name of every flag, file and command, in code font.
- No em dashes in prose, no mid-sentence colons, no hedging adverbs and no hype.
- Say what a thing does or the number it reached, not how it feels.
- Numbered lists for sequences, bullets for everything else.

## Check before committing

1. Run `cargo xtask lint-docs`. It checks dashes, colons, word lists, sentence length, links, record headings and reachability from `README.md`.
2. When rewording, merging or moving pages, run `python3 scripts/factcheck_docs.py <old pages> --also <new pages>`. Every number, link and code span must survive. Explain each reported miss, such as a link rewritten for a moved page.
3. When a page is generated, edit its generator. `benchmarks/silesia/README.md` comes from `scripts/plot_pareto.py`. Regenerate into a temporary copy and confirm the output matches.

## Signs you skipped this

- A second page that tells the same experiment.
- A number in `docs/results.md` that no record contains.
- A verdict that drops the caveat its record states.
