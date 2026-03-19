# λ-Memory — Competitive Research & Landscape Analysis

> What exists, what's novel, what we should steal, and what's ours.

**Status:** Research Complete
**Author:** TEMM1E's Lab
**Date:** 2026-03-15
**Related:** [λ-Memory Design Doc](LAMBDA_MEMORY.md) | [Tem's Mind Architecture](TEMS_MIND_ARCHITECTURE.md)

---

## 1. The Current Landscape

### 1.1 Production Systems (Shipping Today)

| System | Memory Strategy | Decay? | Retrieval | Budget Awareness? |
|--------|----------------|--------|-----------|-------------------|
| **ChatGPT** | Two-tier: explicit saves + full chat history reference | No — binary (exists or deleted) | Keyword/semantic over chat history | No |
| **Google Gemini** | Structured `user_context` document + Always-On Memory Agent (open-sourced via ADK, uses SQLite + LLM consolidation, no vector DB) | No | LLM-driven reads over structured text | No |
| **Claude** | Project-siloed auto-memory + `CLAUDE.md` files | No | Project-scoped search | No |
| **Microsoft Copilot** | Cross-app persistence via OneDrive/Dataverse, GA July 2025 | No | Cross-app entity matching | No |

**Key takeaway:** No production system uses any form of memory decay or importance-weighted forgetting. All use binary retention (present or deleted).

### 1.2 Agent Frameworks

| Framework | Architecture | Decay? | Retrieval | Dynamic Budget? |
|-----------|-------------|--------|-----------|-----------------|
| **Letta/MemGPT** | OS-inspired: core memory (RAM, fixed-size blocks in system prompt) + archival (disk, vector DB). LLM self-manages via function calls. Sleep-time compute for background reorganization. | No — LLM judgment only | Embedding (archival) + keyword (recall) | No — fixed-size core blocks |
| **Mem0** | Hybrid: vector store + graph DB + KV store. LLM-driven fact extraction and conflict resolution (ADD/UPDATE/DELETE/NONE). $24M funded. | No | Vector similarity + optional graph traversal | No |
| **Zep** | Temporal knowledge graph (Graphiti engine). Bi-temporal model with 4 timestamps per edge. Three node types: Episode, Semantic Entity, Community. | Temporal invalidation of edges, not decay | Hybrid: cosine + BM25 + graph BFS + reranking | No — retrieves ~1.6k tokens regardless |
| **LangMem** | Three cognitive types: semantic, episodic, procedural. JSON docs in structured store. Prompt self-modification. | No | Namespace/key filters | No |
| **CrewAI** | Unified Memory class. LLM-analyzed scope/categories/importance at write time. Hierarchical scoping (filesystem-like). | Recency as retrieval signal (not decay) | Composite: similarity + recency + importance | No |
| **AutoGPT** | Removed all vector DBs. JSON file storage. Brute-force on 100k embeddings < milliseconds. | No | Brute-force search | No |

**Key takeaway:** None use decay functions. Letta is closest to our architecture (tiered, self-managing) but uses fixed-size blocks and delegates all prioritization to the LLM. Mem0 and Zep have superior retrieval (vectors, graphs) but no temporal decay model.

### 1.3 Research Papers (Decay-Specific)

| Paper | Year | Approach | Key Innovation |
|-------|------|----------|---------------|
| **FadeMem** ([arXiv:2601.18642](https://arxiv.org/abs/2601.18642)) | Jan 2026 | Adaptive exponential decay with dual layers (LML β=0.8, SML β=1.2) and hysteresis thresholds | Semantic relevance modulates decay — memories about current topic fade slower. 82.1% retention at 55% storage. Beats Mem0 and MemGPT on LTI-Bench. |
| **MemoryBank** ([arXiv:2305.10250](https://arxiv.org/abs/2305.10250)) | AAAI 2024 | Ebbinghaus forgetting curve, discrete memory strength levels | First major paper applying forgetting curves to LLM memory. Recall events adjust strength. |
| **Kore** ([GitHub](https://github.com/auriti-web-design/kore-memory)) | 2025-2026 | Ebbinghaus curve with spaced repetition. Half-life varies by importance (7 days casual, 1 year critical). | Local-only, no LLM for scoring. Retrieval resets clock. |
| **A-MEM** ([arXiv:2502.12110](https://arxiv.org/abs/2502.12110)) | NeurIPS 2025 | Zettelkasten method — structured notes with cross-links, agent creates its own memory operations | Dynamic self-organizing memory, not fixed CRUD |
| **H-MEM** ([arXiv:2507.22925](https://arxiv.org/abs/2507.22925)) | 2025 | Four-layer hierarchy: Domain → Category → Memory Trace → Episode | Layer-by-layer positional filtering |

**Key takeaway:** FadeMem (Jan 2026) is the state of the art for decay-based memory. Their formula is more sophisticated than ours (includes semantic relevance term + hysteresis). MemoryBank proved the concept at AAAI 2024. Kore proved it works locally without embeddings. But **none** combine decay with dynamic context-window budgeting or pre-computed fidelity layers.

### 1.4 Surveys

- [Memory in the Age of AI Agents](https://arxiv.org/abs/2512.13564) (Dec 2025) — Comprehensive taxonomy: token-level vs. parametric vs. latent memory
- [AI Meets Brain](https://arxiv.org/html/2512.23343v1) (Dec 2025) — Bridges cognitive neuroscience and agent memory
- [From Storage to Experience](https://www.preprints.org/manuscript/202601.0618) (Jan 2026) — Three-stage evolution: Storage → Reflection → Experience

---

## 2. Novelty Assessment

### 2.1 What We'd Be Reinventing

| Our Feature | Already Done By | Should We Still Build It? |
|-------------|-----------------|---------------------------|
| Exponential decay on importance | FadeMem, MemoryBank, Kore | **Yes** — it's proven to work, and our implementation context (Rust runtime, integrated skull budgeting) is different enough. But credit the prior art and learn from FadeMem's improvements. |
| Tiered memory (hot/warm/cold) | Letta (RAM/disk), Zep (3 tiers), Mem0 (3 backends) | **Yes** — our tiers are fidelity-based (full/summary/essence), not location-based. Different approach to the same problem. |
| LLM summary at write time | Mem0, CrewAI | **Yes** — standard practice. Our inplace extraction (same API call) is a cost optimization. |
| Recall strengthens memory | Kore, MemoryBank | **Yes** — biological reconsolidation is well-established. Our hash-based recall mechanism is novel. |

### 2.2 What's Genuinely Novel (Not Found in Any System)

**1. Hash-based recall from compressed memory**

No system — research or production — gives the agent **awareness of faded memories via identifiers** with selective full-content retrieval. Every other system uses vector search, keyword search, or graph traversal. The agent either finds a memory or it doesn't. In λ-Memory, Tem **sees the shape of what it forgot** and can choose to recall it. This is unexplored territory.

Closest analog: Kore uses content-analysis-based scoring, but retrieval is still keyword-matching, not identifier-based.

**2. Dynamic budget derived from model context window**

No system automatically adjusts memory allocation based on the model's context window at runtime. Letta uses fixed-size core memory blocks. Zep retrieves ~1.6k tokens regardless. Mem0 returns top-k with no context awareness.

Our formula: `skull - bone - active - output_reserve - guard = memory_budget` with adaptive pressure thresholds is unique. It means the same algorithm works correctly on a 16k local model and a 2M Grok context window.

**3. Pre-computed fidelity layers with score-based selection**

Three representations (full/summary/essence) written at creation time, with the packing algorithm selecting which representation to show based on current decay score + available budget. FadeMem has dual layers but both store full-resolution content — they just move between long-term and short-term storage. Nobody pre-computes multiple compression levels and dynamically selects at read time.

### 2.3 Where We're Weaker (Honest Gaps)

| Gap | Who Does It Better | Severity | Our Mitigation |
|-----|-------------------|----------|----------------|
| No vector/semantic retrieval | Mem0, Zep, Letta archival | Medium | SQLite FTS5 on LLM-generated tags/summaries — 80% quality, 0% cost. See §3. |
| No relational links between memories | Zep (knowledge graph), Mem0 (graph mode) | Low-Medium | Tags provide implicit grouping. Could add explicit links later. |
| Decay function lacks semantic relevance term | FadeMem | Medium | FTS5 BM25 score as relevance proxy — boosts topically relevant memories. |
| No background memory reorganization | Letta (sleep-time compute) | Low | Inplace extraction is simpler and doesn't block. Could add async consolidation later. |
| No hysteresis on tier boundaries | FadeMem | Low | Can add if oscillation observed in practice. |

---

## 3. Retrieval Without Embeddings

Embedding models would add 80MB-1.3GB of dependencies and require significant compute. TEMM1E is a lean Rust binary with zero ML model dependencies. We preserve this.

**Approach: SQLite FTS5 on LLM-generated semantic text.**

At creation time, the LLM already produces `summary`, `essence`, and `tags`. These are high-quality semantic compressions. FTS5 provides BM25 ranking (term frequency / inverse document frequency) over these fields.

```sql
CREATE VIRTUAL TABLE gm_fts USING fts5(
    summary, essence, tags,
    content='lambda_memories',
    content_rowid='rowid'
);
```

**Hybrid scoring:**
```
retrieval_score = α × decay_score + β × bm25_relevance
```

This is structurally equivalent to FadeMem's formula:
```
I(t) = α·relevance(memory, query) + β·frequency/(1+frequency) + γ·recency(t)
```

But ours uses BM25 on pre-extracted semantic text instead of embedding cosine similarity. The tradeoff: BM25 misses synonyms that vectors catch. But since the tags/summaries were LLM-generated from the same conversation, vocabulary alignment is naturally high.

**Zero new dependencies. Zero additional API calls. Zero model files.**

---

## 4. Summary Comparison Table

| Feature | ChatGPT | Gemini | Letta | Mem0 | Zep | FadeMem | **TEMM1E Gradient** |
|---------|---------|--------|-------|------|-----|---------|---------------------|
| Memory decay | - | - | - | - | Temporal invalidation | Exponential (dual-layer) | Exponential (single, with FTS5 relevance) |
| Tiered storage | - | - | RAM/disk | Vector+Graph+KV | 3 node types | Dual-layer | **3 fidelity layers** (full/summary/essence) |
| Dynamic context budget | - | - | - | - | - | - | **Yes** (skull model) |
| Hash-based recall | - | - | - | - | - | - | **Yes** |
| Semantic retrieval | Chat history | LLM-driven | Embedding + keyword | Vector + graph | Hybrid (5 methods) | Embedding | **FTS5 on LLM-extracted text** |
| Zero ML dependency | Yes (API) | Yes (API) | Needs embedding model | Needs embedding model | Needs Neo4j + embeddings | Needs embedding model | **Yes** |
| Background processing | - | Always-On Agent | Sleep-time compute | - | Incremental graph | - | - |
| Recall strengthens memory | - | - | - | - | - | Access frequency term | **Yes** (last_accessed reset) |

---

## 5. Key Sources

### Papers
- FadeMem: [arXiv:2601.18642](https://arxiv.org/abs/2601.18642) — Biologically-inspired forgetting for efficient agent memory
- MemGPT: [arXiv:2310.08560](https://arxiv.org/abs/2310.08560) — Towards LLMs as Operating Systems
- Zep/Graphiti: [arXiv:2501.13956](https://arxiv.org/abs/2501.13956) — Temporal knowledge graphs for agent memory
- Mem0: [arXiv:2504.19413](https://arxiv.org/abs/2504.19413) — Memory layer for AI agents
- MemoryBank: [arXiv:2305.10250](https://arxiv.org/abs/2305.10250) — Ebbinghaus curve for LLM memory (AAAI 2024)
- A-MEM: [arXiv:2502.12110](https://arxiv.org/abs/2502.12110) — Agentic Memory (NeurIPS 2025)
- Memory in the Age of AI Agents: [arXiv:2512.13564](https://arxiv.org/abs/2512.13564)

### Production Systems
- [OpenAI Memory](https://openai.com/index/memory-and-new-controls-for-chatgpt/)
- [Google Always-On Memory Agent](https://github.com/GoogleCloudPlatform/generative-ai/tree/main/gemini/agents/always-on-memory-agent)
- [Claude Memory](https://docs.anthropic.com/en/docs/claude-code/memory)
- [Microsoft Copilot Memory](https://techcommunity.microsoft.com/blog/microsoft365copilotblog/introducing-copilot-memory-a-more-productive-and-personalized-ai-for-the-way-you/4432059)

### Frameworks
- [Letta Docs](https://docs.letta.com/core-concepts/) | [Sleep-Time Compute](https://www.letta.com/blog/sleep-time-compute) | [Memory Blocks](https://www.letta.com/blog/memory-blocks)
- [Mem0 GitHub](https://github.com/mem0ai/mem0) | [Research](https://mem0.ai/research)
- [Zep / Graphiti](https://www.getzep.com/) | [SOTA Blog](https://blog.getzep.com/state-of-the-art-agent-memory/)
- [Kore Memory](https://github.com/auriti-web-design/kore-memory)
- [LangMem SDK](https://blog.langchain.com/langmem-sdk-launch/)
- [CrewAI Memory](https://docs.crewai.com/en/concepts/memory)
