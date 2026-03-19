# TEMM1E Tem's Mind Architecture

> The cognitive engine behind TEMM1E's autonomous execution.

---

## The Execution Cycle

```
                          ┌─────────────────────────────────────────────────┐
                          │              PROACTIVE TRIGGERS                 │
                          │  FileChanged · CronSchedule · Webhook · Metric  │
                          └────────────────────┬────────────────────────────┘
                                               │ (opt-in, rate-limited)
                                               ▼
┌──────────┐    ┌──────────────────────────────────────────────────────────────────┐
│          │    │                        AGENT RUNTIME                             │
│  USER    │    │                                                                  │
│ MESSAGE  │───▶│   ORDER ──▶ THINK ──▶ ACTION ──▶ VERIFY ──┐                     │
│          │    │                                             │                     │
│ Telegram │    │             ┌───────────────────────────────┘                     │
│ Discord  │    │             │                                                    │
│ Slack    │    │             ├── DONE? ──▶ yes ──▶ LEARN ──▶ REPORT ──▶ END      │
│ CLI      │    │             │                                                    │
│          │    │             └── no ──▶ THINK ──▶ ACTION ──▶ VERIFY ──▶ ...      │
│          │    │                                                                  │
└──────────┘    └──────────────────────────────────────────────────────────────────┘
```

---

## Component Map

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                           temm1e-agent crate                                   │
│                                                                                 │
│  ┌─────────────────────────────────────────────────────────────────────────┐    │
│  │                         RUNTIME (runtime.rs)                            │    │
│  │                                                                         │    │
│  │  AgentRuntime.process_message()                                         │    │
│  │    │                                                                    │    │
│  │    ├── Circuit Breaker ────── gate provider calls                       │    │
│  │    ├── DONE Criteria ──────── detect compound tasks                     │    │
│  │    ├── Task Queue ─────────── checkpoint after each round               │    │
│  │    ├── Self-Correction ────── rotate strategy on repeated failure       │    │
│  │    ├── Verification ──────── inject verify hint after every tool        │    │
│  │    └── Learning ───────────── extract & persist lessons on completion   │    │
│  │                                                                         │    │
│  └─────────────────────────────────────────────────────────────────────────┘    │
│                                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐      │
│  │  CONTEXT MANAGER  │  │  MODEL ROUTER    │  │  PROMPT OPTIMIZER        │      │
│  │  (context.rs)     │  │  (model_router)  │  │  (prompt_optimizer.rs)   │      │
│  │                   │  │                  │  │                          │      │
│  │  Token budgeting: │  │  Task complexity │  │  SystemPromptBuilder     │      │
│  │  1. System prompt │  │  analysis:       │  │  Composable prompt       │      │
│  │  2. Tool defs     │  │                  │  │  construction with       │      │
│  │  3. Task state    │  │  Simple → Fast   │  │  workspace, tools,      │      │
│  │  4. Recent msgs   │  │  Medium → Std    │  │  verification rules     │      │
│  │  5. Memory (15%)  │  │  Complex → Prem  │  │                          │      │
│  │  6. Learnings (5%)│  │                  │  │  + Prompt Patches        │      │
│  │  7. Older history │  │  ModelTier enum  │  │    (self-tuning)         │      │
│  └──────────────────┘  └──────────────────┘  └──────────────────────────┘      │
│                                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐      │
│  │  EXECUTOR         │  │  DELEGATION      │  │  TASK DECOMPOSITION      │      │
│  │  (executor.rs)    │  │  (delegation.rs) │  │  (task_decomposition.rs) │      │
│  │                   │  │                  │  │                          │      │
│  │  execute_tool()   │  │  DelegationMgr   │  │  TaskGraph with          │      │
│  │  execute_parallel │  │  SubAgent spawn  │  │  dependency edges        │      │
│  │  detect_deps()    │  │  Result aggreg.  │  │  Topological sort        │      │
│  │  Semaphore-based  │  │  No recursion    │  │  Cycle detection         │      │
│  │  concurrency      │  │  Scoped tools    │  │  Status tracking         │      │
│  └──────────────────┘  └──────────────────┘  └──────────────────────────┘      │
│                                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐      │
│  │  SELF-CORRECTION  │  │  HISTORY PRUNING │  │  OUTPUT COMPRESSION      │      │
│  │  (self_correct.)  │  │  (history_prun.) │  │  (output_compress.)      │      │
│  │                   │  │                  │  │                          │      │
│  │  FailureTracker   │  │  score_message() │  │  Compress large tool     │      │
│  │  per-tool failure │  │  MessageImport.: │  │  outputs before storing  │      │
│  │  count tracking   │  │  Critical/High/  │  │  in context. Extract     │      │
│  │  Strategy rotation│  │  Medium/Low      │  │  key info, discard       │      │
│  │  after N failures │  │  prune_history() │  │  verbose noise           │      │
│  └──────────────────┘  └──────────────────┘  └──────────────────────────┘      │
│                                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐      │
│  │  PROACTIVE        │  │  PROMPT PATCHES  │  │  LEARNING                │      │
│  │  (proactive.rs)   │  │  (prompt_patch.) │  │  (learning.rs)           │      │
│  │                   │  │                  │  │                          │      │
│  │  TriggerRule sys  │  │  Self-tuning:    │  │  extract_learnings()     │      │
│  │  FileChanged      │  │  ToolUsageHint   │  │  from completed tasks    │      │
│  │  CronSchedule     │  │  ErrorAvoidance  │  │                          │      │
│  │  Webhook          │  │  WorkflowPattern │  │  TaskLearning:           │      │
│  │  Threshold        │  │  DomainKnowledge │  │  task_type, approach,    │      │
│  │  Rate limiting    │  │  StylePreference │  │  outcome, lesson         │      │
│  │  Opt-in required  │  │  User approval   │  │  Stored in memory        │      │
│  └──────────────────┘  └──────────────────┘  └──────────────────────────┘      │
│                                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐      │
│  │  CIRCUIT BREAKER  │  │  TASK QUEUE      │  │  STREAMING               │      │
│  │  (circuit_break.) │  │  (task_queue.rs) │  │  (streaming.rs)          │      │
│  │                   │  │                  │  │                          │      │
│  │  Closed → Open    │  │  SQLite-backed   │  │  StreamBuffer            │      │
│  │  → Half-Open      │  │  TaskEntry CRUD  │  │  StreamingConfig         │      │
│  │  Exp. backoff     │  │  Checkpointing   │  │  StreamingNotifier       │      │
│  │  with jitter      │  │  Survives crash  │  │  Edit-in-place on TG     │      │
│  └──────────────────┘  └──────────────────┘  └──────────────────────────┘      │
│                                                                                 │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────────┐      │
│  │  WATCHDOG         │  │  RECOVERY        │  │  STARTUP                 │      │
│  │  (watchdog.rs)    │  │  (recovery.rs)   │  │  (startup.rs)            │      │
│  │                   │  │                  │  │                          │      │
│  │  Monitor provider │  │  RecoveryManager │  │  StartupMetrics          │      │
│  │  memory, channel, │  │  RecoveryPlan    │  │  LazyResource<T>         │      │
│  │  tools subsystems │  │  Restart/Rollback│  │  Async lazy init         │      │
│  │  HealthReport     │  │  /Skip/Escalate  │  │  Health pre-checks       │      │
│  └──────────────────┘  └──────────────────┘  └──────────────────────────┘      │
│                                                                                 │
│  ┌──────────────────┐                                                          │
│  │  DONE CRITERIA    │                                                          │
│  │  (done_criteria)  │                                                          │
│  │                   │                                                          │
│  │  Compound task    │                                                          │
│  │  detection        │                                                          │
│  │  DONE prompt inj. │                                                          │
│  │  Verify on reply  │                                                          │
│  └──────────────────┘                                                          │
│                                                                                 │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Data Flow: A Single Message

```
User sends "Deploy the app, run migrations, and verify health"
                              │
                              ▼
┌─ 1. ORDER ──────────────────────────────────────────────────────────────────┐
│                                                                              │
│  InboundMessage arrives via Channel (Telegram/Discord/Slack/CLI)             │
│  AgentRuntime.process_message() begins                                       │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────┐            │
│  │ DONE Criteria Engine detects compound task (3 verbs + "and") │            │
│  │ Injects: "Define DONE conditions for each sub-task"          │            │
│  └──────────────────────────────────────────────────────────────┘            │
│                                                                              │
│  ┌──────────────────────────────────────────────────────────────┐            │
│  │ Task Queue creates persistent entry (SQLite)                 │            │
│  │ Status: Pending → Running                                    │            │
│  └──────────────────────────────────────────────────────────────┘            │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─ 2. THINK ──────────────────────────────────────────────────────────────────┐
│                                                                              │
│  Context Manager assembles CompletionRequest:                                │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────┐                │
│  │ Budget: 30,000 tokens                                   │                │
│  │                                                         │                │
│  │ [1] System prompt ............ 800 tokens  (always)     │                │
│  │     + Prompt Patches ......... 200 tokens  (approved)   │                │
│  │ [2] Tool definitions ......... 1,200 tokens (always)    │                │
│  │ [3] DONE criteria ............ 150 tokens  (injected)   │                │
│  │ [4] Recent messages .......... 3,000 tokens (4-8 msgs)  │                │
│  │ [5] Memory search ............ 4,500 tokens (15% cap)   │                │
│  │ [6] Cross-task learnings ...... 1,500 tokens (5% cap)   │                │
│  │ [7] Older history ............ fills remainder           │                │
│  │                                                         │                │
│  │ Dropped messages get summary injection:                 │                │
│  │ "[Earlier context: 12 msgs dropped, discussed: ...]"    │                │
│  └─────────────────────────────────────────────────────────┘                │
│                                                                              │
│  Model Router selects tier based on task complexity                          │
│  Circuit Breaker checks: can we call the provider?                          │
│                                                                              │
│  Provider.complete(request) → CompletionResponse                            │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─ 3. ACTION ─────────────────────────────────────────────────────────────────┐
│                                                                              │
│  Response contains tool_use calls                                            │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────┐                │
│  │ Executor: Are these tools independent?                  │                │
│  │                                                         │                │
│  │   detect_dependencies() → union-find grouping           │                │
│  │   • read-read → independent → parallel                  │                │
│  │   • write-write → dependent → sequential                │                │
│  │   • shell → always dependent                            │                │
│  │                                                         │                │
│  │   execute_tools_parallel() with Semaphore(max=5)        │                │
│  └─────────────────────────────────────────────────────────┘                │
│                                                                              │
│  For complex sub-tasks, DelegationManager may spawn SubAgents:              │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────┐                │
│  │ Primary Agent                                           │                │
│  │   ├── SubAgent A: "Deploy the app"     (shell tools)    │                │
│  │   ├── SubAgent B: "Run migrations"     (shell, file)    │                │
│  │   └── SubAgent C: "Verify health"      (web_fetch)      │                │
│  │                                                         │                │
│  │ Each sub-agent has:                                     │                │
│  │   • Scoped objective + scoped tools                     │                │
│  │   • Own timeout + max_rounds                            │                │
│  │   • Cheaper model (optional)                            │                │
│  │   • CANNOT spawn further sub-agents (no recursion)      │                │
│  └─────────────────────────────────────────────────────────┘                │
│                                                                              │
│  Tool output is compressed if large (OutputCompression)                     │
│  Truncated at 30,000 chars with "[Output truncated]" marker                 │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─ 4. VERIFY ─────────────────────────────────────────────────────────────────┐
│                                                                              │
│  Verification Engine injects hint into tool result:                          │
│                                                                              │
│    "[VERIFICATION REQUIRED] Review the tool output(s) above.                │
│     1. Did the action succeed? What evidence confirms this?                 │
│     2. If it failed, what went wrong? Do NOT retry the same approach.       │
│     3. If uncertain, use a tool to verify."                                 │
│                                                                              │
│  Self-Correction Engine tracks failures:                                     │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────┐                │
│  │ FailureTracker                                          │                │
│  │   tool: "shell" → failures: 2 → THRESHOLD EXCEEDED     │                │
│  │                                                         │                │
│  │   Inject: "[STRATEGY ROTATION] This approach has failed │                │
│  │   2 times. Analyze why. Try a fundamentally different   │                │
│  │   approach. Previous errors: ..."                       │                │
│  └─────────────────────────────────────────────────────────┘                │
│                                                                              │
│  Pending user messages injected into last tool result                        │
│  Task Queue checkpoints session state (SQLite)                               │
│                                                                              │
│  Loop back to THINK if more tool calls needed                               │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─ 5. DONE ───────────────────────────────────────────────────────────────────┐
│                                                                              │
│  No more tool calls → final text reply                                       │
│                                                                              │
│  DONE Criteria verification appended for compound tasks                      │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────┐                │
│  │ Cross-Task Learning                                     │                │
│  │                                                         │                │
│  │  extract_learnings(history) →                           │                │
│  │    TaskLearning {                                       │                │
│  │      task_type: "shell+web",                            │                │
│  │      approach: ["shell", "web_fetch", "shell"],         │                │
│  │      outcome: Success,                                  │                │
│  │      lesson: "Deploy succeeded using shell → web_fetch  │                │
│  │               → shell. Verify health with HTTP check."  │                │
│  │    }                                                    │                │
│  │                                                         │                │
│  │  Stored in memory with "learning:" prefix               │                │
│  │  Retrieved in future context (5% token budget)          │                │
│  └─────────────────────────────────────────────────────────┘                │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────┐                │
│  │ Prompt Patch Extraction                                 │                │
│  │                                                         │                │
│  │  extract_prompt_patches(history, tools_used) →          │                │
│  │    PromptPatch {                                        │                │
│  │      type: WorkflowPattern,                             │                │
│  │      content: "For deploy tasks: shell → web_fetch      │                │
│  │               → shell to verify",                       │                │
│  │      confidence: 0.7,                                   │                │
│  │      status: Proposed (needs user approval)             │                │
│  │    }                                                    │                │
│  │                                                         │                │
│  │  If approved → injected into future system prompts      │                │
│  │  If underperforming → auto-expired                      │                │
│  └─────────────────────────────────────────────────────────┘                │
│                                                                              │
│  Task Queue marks task Completed                                            │
│  OutboundMessage sent via Channel                                           │
│                                                                              │
└──────────────────────────────────────────────────────────────────────────────┘
```

---

## Safety & Resilience Layer

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         SAFETY & RESILIENCE                                 │
│                                                                             │
│  ┌─────────────┐  ┌─────────────────┐  ┌────────────────┐                 │
│  │   CIRCUIT    │  │    WATCHDOG      │  │   RECOVERY     │                 │
│  │   BREAKER    │  │                  │  │   MANAGER      │                 │
│  │              │  │  Monitors:       │  │                │                 │
│  │  ┌────────┐  │  │  • Provider      │  │  Detects:      │                 │
│  │  │ CLOSED │──┼──│  • Memory        │  │  • Corrupted   │                 │
│  │  └───┬────┘  │  │  • Channel       │  │    state       │                 │
│  │  N failures  │  │  • Tools         │  │  • Orphaned    │                 │
│  │  ┌───▼────┐  │  │                  │  │    tasks       │                 │
│  │  │  OPEN  │  │  │  HealthReport    │  │                │                 │
│  │  └───┬────┘  │  │  per subsystem   │  │  Actions:      │                 │
│  │  cooldown    │  │  Auto-restart    │  │  • Restart     │                 │
│  │  ┌───▼────┐  │  │  degraded subs   │  │  • Rollback    │                 │
│  │  │ HALF-  │  │  │                  │  │  • Skip        │                 │
│  │  │ OPEN   │  │  │  Feeds into      │  │  • Escalate    │                 │
│  │  └────────┘  │  │  heartbeat       │  │                │                 │
│  └─────────────┘  └─────────────────┘  └────────────────┘                 │
│                                                                             │
│  ┌─────────────┐  ┌─────────────────┐  ┌────────────────┐                 │
│  │  TASK QUEUE  │  │   PROACTIVE     │  │  DELEGATION    │                 │
│  │  CHECKPOINT  │  │   SAFETY        │  │  SAFETY        │                 │
│  │              │  │                  │  │                │                 │
│  │  After every │  │  • Disabled by   │  │  • Max 10 sub- │                 │
│  │  tool round: │  │    default       │  │    agents/task │                 │
│  │  serialize   │  │  • 10 acts/hour  │  │  • Max 3       │                 │
│  │  session to  │  │  • Per-rule      │  │    concurrent  │                 │
│  │  SQLite      │  │    cooldowns     │  │  • No recursion│                 │
│  │              │  │  • Destructive   │  │    (sub-agents │                 │
│  │  Survives    │  │    ops require   │  │    cannot      │                 │
│  │  crash →     │  │    confirmation  │  │    delegate)   │                 │
│  │  resume on   │  │  • Audit log     │  │  • Own timeout │                 │
│  │  restart     │  │    (tracing)     │  │    per agent   │                 │
│  └─────────────┘  └─────────────────┘  └────────────────┘                 │
│                                                                             │
│  ┌─────────────────────────────────────────────────────────────────┐       │
│  │  PROMPT PATCH SAFETY                                            │       │
│  │                                                                 │       │
│  │  All patches start as Proposed → need Approval to activate      │       │
│  │  Auto-approve ONLY for low-risk types above 0.8 confidence      │       │
│  │  ErrorAvoidance/WorkflowPattern ALWAYS need manual approval     │       │
│  │  Underperforming patches auto-expire after N applications       │       │
│  │  Max 20 patches to prevent prompt bloat                         │       │
│  └─────────────────────────────────────────────────────────────────┘       │
│                                                                             │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Module Dependency Graph

```
                              runtime.rs
                           (AgentRuntime)
                                 │
              ┌──────────┬───────┼───────┬──────────┬──────────┐
              │          │       │       │          │          │
              ▼          ▼       ▼       ▼          ▼          ▼
         context.rs  executor  done_  circuit_  learning  task_queue
              │       .rs      crit.  breaker     .rs       .rs
              │        │       .rs      .rs        │
              │        │                           │
     ┌────────┼────────┤                           │
     │        │        │                           │
     ▼        ▼        ▼                           ▼
  prompt_  history_  output_                   prompt_
  optim.   pruning   compress.                 patches.rs
  .rs      .rs       .rs

                    STANDALONE MODULES
           (no internal dependencies, used by runtime)

  ┌──────────┐  ┌───────────┐  ┌───────────┐  ┌────────────┐
  │ model_   │  │ self_     │  │ streaming │  │ task_      │
  │ router   │  │ correction│  │ .rs       │  │ decomp.   │
  │ .rs      │  │ .rs       │  │           │  │ .rs       │
  └──────────┘  └───────────┘  └───────────┘  └────────────┘

  ┌──────────┐  ┌───────────┐  ┌───────────┐  ┌────────────┐
  │ watchdog │  │ recovery  │  │ proactive │  │ delegation │
  │ .rs      │  │ .rs       │  │ .rs       │  │ .rs        │
  └──────────┘  └───────────┘  └───────────┘  └────────────┘

  ┌──────────┐
  │ startup  │
  │ .rs      │
  └──────────┘
```

---

## Self-Improvement Loop

```
                    Task N completes
                          │
                          ▼
              ┌───────────────────────┐
              │  extract_learnings()  │──── What tools worked? What failed?
              │  extract_patches()    │──── What prompt change would help?
              └───────────┬───────────┘
                          │
                ┌─────────┴─────────┐
                ▼                   ▼
        ┌──────────────┐   ┌──────────────┐
        │  TaskLearning │   │ PromptPatch  │
        │  stored in    │   │ status:      │
        │  Memory with  │   │  Proposed    │
        │  "learning:"  │   │              │
        │  prefix       │   │ User reviews │
        └──────┬───────┘   │ /patches cmd │
               │           └──────┬───────┘
               │                  │ approve/reject
               │                  ▼
               │           ┌──────────────┐
               │           │  Approved    │
               │           │  patches     │
               │           └──────┬───────┘
               │                  │
               ▼                  ▼
        ┌─────────────────────────────────┐
        │     Task N+1 starts             │
        │                                 │
        │  Context Manager injects:       │
        │  • Past learnings (5% budget)   │
        │  • Active prompt patches        │
        │                                 │
        │  Agent performs better on        │
        │  similar tasks over time         │
        └─────────────────────────────────┘
               │
               ▼
        ┌─────────────────────────────────┐
        │  record_task_outcome()          │
        │  Update patch success_rate      │
        │                                 │
        │  If success_rate < threshold    │
        │  after N applications:          │
        │  → expire_underperforming()     │
        │  → patch auto-removed           │
        └─────────────────────────────────┘
```

---

## External Integration Points

```
┌───────────────┐     ┌─────────────────┐     ┌──────────────────┐
│   CHANNELS    │     │    PROVIDERS     │     │     MEMORY       │
│               │     │                  │     │                  │
│  Telegram ────┤     │  Anthropic ──────┤     │  SQLite ─────────┤
│  Discord  ────┤     │  OpenAI-compat ──┤     │  Markdown ───────┤
│  Slack    ────┤     │                  │     │                  │
│  CLI      ────┤     │  Circuit Breaker │     │  Hybrid search:  │
│               │     │  gates all calls │     │  vector(0.7) +   │
│  Channel trait│     │  Provider trait   │     │  keyword(0.3)    │
└───────┬───────┘     └────────┬─────────┘     └────────┬─────────┘
        │                      │                         │
        └──────────────────────┼─────────────────────────┘
                               │
                     ┌─────────▼──────────┐
                     │   AGENT RUNTIME    │
                     │   (temm1e-agent)  │
                     └─────────┬──────────┘
                               │
        ┌──────────────────────┼─────────────────────────┐
        │                      │                         │
┌───────▼───────┐     ┌───────▼──────────┐     ┌────────▼─────────┐
│    TOOLS      │     │     VAULT        │     │    GATEWAY       │
│               │     │                  │     │                  │
│  shell ───────┤     │  ChaCha20-Poly   │     │  axum HTTP/WS    │
│  file_read ───┤     │  1305 encryption │     │  /health         │
│  file_write ──┤     │  vault:// URIs   │     │  /dashboard      │
│  browser  ────┤     │                  │     │  /auth/callback   │
│  web_fetch ───┤     │  Vault trait     │     │  Session mgmt    │
│  send_msg ────┤     │                  │     │  OAuth identity  │
│  send_file ───┤     │                  │     │                  │
│  check_msgs ──┤     │                  │     │                  │
│               │     │                  │     │                  │
│  Tool trait   │     │                  │     │                  │
└───────────────┘     └──────────────────┘     └──────────────────┘
```

---

## Key Design Principles

| Principle | Implementation |
|-----------|---------------|
| **No blind execution** | Verification hint injected after every tool call |
| **No context bloat** | 7-tier priority budgeting, history pruning, output compression |
| **No silent failure** | FailureTracker + strategy rotation + circuit breaker |
| **No premature completion** | DONE criteria engine for compound tasks |
| **No rigid plans** | Self-correction rotates strategy after repeated failures |
| **No wasted experience** | Cross-task learning + prompt patches persist knowledge |
| **No uncontrolled autonomy** | Proactive triggers disabled by default, prompt patches need approval |
| **No single point of failure** | Circuit breaker, memory failover, task queue checkpointing |
