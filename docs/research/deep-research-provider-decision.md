# Deep Research Provider Decision

Phase 05 uses `langchain-ai/local-deep-researcher` as the loop pattern for
zetesis-owned Rust orchestration. Zetesis does not vendor the Python runtime.

## Decision

Implement the eventual `LocalDeepResearch` backend as a Rust loop with these
nodes:

1. `generate_query`
2. `web_research`
3. `summarize_sources`
4. `reflect_on_summary`
5. `finalize_summary`

The loop runs against `sylloge::DeepResearch`'s task lifecycle:
`submit -> poll -> fetch`. The Phase 1 scaffold already exposes that trait; the
backend now has an in-memory `LocalDeepResearch` task lifecycle scaffold. It
now also has an offline fixture layer that exercises the five-node loop against
deterministic query-generation, source-retrieval, synthesis, reflection, and
finalization seams. It still needs real local LLM binding, search configuration,
network-backed DuckDuckGo/SearXNG integration, and endpoint-backed integration
tests.

## Source evidence

Evidence was checked against upstream source snapshots on 2026-05-25.

| Candidate | Snapshot | Disposition | Evidence |
|-----------|----------|-------------|----------|
| `langchain-ai/local-deep-researcher` | `e172109` | Vendor the loop pattern. | `configuration.py` defaults to local Ollama and DuckDuckGo, with LMStudio and SearXNG supported. `graph.py` defines the five nodes and routes `reflect_on_summary` back to `web_research` until loop depth is exhausted. |
| `assafelovic/gpt-researcher` | `92bfc03` | Reuse the adapter shape only. | The generic LLM registry includes `vllm_openai`, wired through `VLLM_OPENAI_API_KEY` and `VLLM_OPENAI_API_BASE`. That maps cleanly to logismos or llama.cpp OpenAI-compatible endpoints. |
| `langchain-ai/open_deep_research` | `4b61120` | Do not adopt. | `SearchAPI` is limited to `anthropic`, `openai`, `tavily`, and `none`, with Tavily as the default. Its model defaults are OpenAI model strings. That search/model default is the wrong sovereignty posture for zetesis. |

## Implementation notes

- Default inference target: logismos-compatible OpenAI endpoint, initially
  expected at `http://127.0.0.1:8000/v1`.
- Default search target: DuckDuckGo. SearXNG is the preferred operator-hosted
  override.
- `gpt-researcher` is not the backend contract. Its useful takeable is the
  `vllm_openai` adapter convention; its retriever registry is still a static
  `VALID_RETRIEVERS` list, so custom retrievers require patching or an interim
  local-document path such as `DOC_PATH`.
- Deep-research outputs must be converted into the existing
  `ResearchResult` envelope: cited hits, provenance entries, cost ledger, and a
  stable cache key. Do not add a parallel `{ summary, citations,
  knowledge_gaps }` result type unless the public API is deliberately revised.

## Remaining backend work

The remaining non-fixture `LocalDeepResearch` implementation should include:

- real OpenAI-compatible local LLM binding for query generation, summarization,
  reflection, and finalization;
- DuckDuckGo/SearXNG search client integration behind the existing offline seams;
- endpoint-backed assertions that every final result carries citations,
  provenance, and budget accounting;
- timeout, cancellation, and retry behavior for non-fixture execution.
