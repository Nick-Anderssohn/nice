# Judge prompt template (3 independent claude-fable-5 judges per T1/T2 artifact)

Placeholders in {BRACES} are filled per artifact. Judges are BLIND: no mention
of experiment/comparison/other arm/rewrite; input is one session's artifacts.
Judges get NO tools-needed task — everything is inlined; instruct them not to
explore the repo.

---

You are grading ONE completed feature-implementation session by a coding agent, from its artifacts. This is an absolute assessment against the anchored rubric below — there is nothing to compare it to and no context beyond what is given here. Do not explore the filesystem; judge only from the materials in this prompt.

## Feature request the agent was given

{BRIEF}

## Final diff (the agent's entire change)

```diff
{DIFF}
```

## Canonical build log of the final tree (tail)

```
{BUILD_LOG_TAIL}
```

## Tool-call sequence (chronological; seq, tool, input excerpt)

```
{TOOLCALLS}
```

## Objective verification results (independent verifier drove the running app)

{OBJECTIVE}

## Rubric — score each dimension 1–5 (2/4 = interpolate between anchors)

| Dimension | 5 | 3 | 1 |
|---|---|---|---|
| Edit locality | touches only files a maintainer would expect | one or two gratuitous excursions | sprawling/refactors unrelated code |
| API hallucination count | 0 invented APIs across the session | 1–2, self-corrected via compiler | ≥5, or one survives into the final diff |
| Iterations-to-green | ≤2 build/run cycles to done | 3–5 | >8 or never green |
| Human-fixup minutes (your estimate to make this mergeable) | <5 min | 15–30 min | >60 min |
| Style conformance | indistinguishable from surrounding code's idiom | recognizably foreign but acceptable | fights the house patterns |

Counting rules: an API hallucination = a compile/run error citing a symbol, method, or API the agent invented (does not exist); a misremembered argument order/name on a REAL API counts half-weight. Iterations-to-green = number of build/run cycles (from the tool-call sequence) until the final working state. Count first, then map to the anchor.

## Output format (exactly this, nothing else)

```
edit_locality: <score> — <2-3 sentence rationale citing evidence (files, seq numbers, log lines)>
api_hallucination: <score> — <count + rationale>
iterations_to_green: <score> — <count + rationale>
human_fixup_minutes: <score> — <estimated minutes + rationale>
style_conformance: <score> — <rationale>
composite: <mean of the five scores, one decimal>
```
