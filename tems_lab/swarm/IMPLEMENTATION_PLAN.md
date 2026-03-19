# TEMM1E Hive — Implementation Plan

## Reference Document for Implementation

**Purpose:** This is the build guide. Every struct, function, file, and test is specified here. Follow this document top-to-bottom during implementation.

---

## Phase 1: Crate Scaffold + Core Types

### 1.1 Create `crates/temm1e-hive/`

```
crates/temm1e-hive/
├── Cargo.toml
├── src/
│   ├── lib.rs              # Hive struct, public API
│   ├── blackboard.rs       # SQLite task DAG (Blackboard)
│   ├── pheromone.rs        # Pheromone field (signal store)
│   ├── selection.rs        # Worker task selection equation
│   ├── queen.rs            # Decomposition function
│   ├── worker.rs           # Worker execution loop
│   ├── dag.rs              # DAG validation + critical path
│   ├── types.rs            # Shared types (HiveTask, Pheromone, etc.)
│   └── config.rs           # HiveConfig (re-exported from here)
└── tests/
    └── (inline #[cfg(test)] modules)
```

### 1.2 Cargo.toml

```toml
[package]
name = "temm1e-hive"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[dependencies]
temm1e-core = { path = "../temm1e-core" }
tokio = { workspace = true }
sqlx = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
uuid = { version = "1", features = ["v4"] }
chrono = { version = "0.4", features = ["serde"] }
rand = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["test-util"] }
```

### 1.3 Types (`src/types.rs`)

```rust
// HiveTaskStatus — matches DESIGN.md §3.1
pub enum HiveTaskStatus {
    Pending,    // waiting for dependencies
    Ready,      // dependencies met, available for claim
    Active,     // claimed by a worker
    Complete,   // verified done
    Blocked,    // worker hit a problem
    Retry,      // being re-attempted
    Escalate,   // exceeded retries, needs fallback
}

// HiveTask — a single decomposed subtask
pub struct HiveTask {
    pub id: String,
    pub order_id: String,
    pub description: String,
    pub status: HiveTaskStatus,
    pub claimed_by: Option<String>,
    pub dependencies: Vec<String>,      // task IDs
    pub context_tags: Vec<String>,
    pub estimated_tokens: u32,
    pub actual_tokens: u32,
    pub result_summary: Option<String>,
    pub artifacts: Vec<String>,
    pub retry_count: u32,
    pub max_retries: u32,
    pub error_log: Option<String>,
    pub created_at: i64,                // unix epoch ms
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

// HiveOrder — tracks the overall request
pub struct HiveOrder {
    pub id: String,
    pub chat_id: String,
    pub original_message: String,
    pub task_count: usize,
    pub completed_count: usize,
    pub status: HiveOrderStatus,       // Active, Completed, Failed
    pub total_tokens: u64,
    pub queen_tokens: u64,
    pub created_at: i64,
    pub completed_at: Option<i64>,
}

// PheromoneSignal — one signal in the field
pub struct PheromoneSignal {
    pub id: i64,
    pub signal_type: SignalType,
    pub target: String,
    pub intensity: f64,
    pub decay_rate: f64,
    pub emitter: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub created_at: i64,
}

pub enum SignalType {
    Completion,
    Failure,
    Difficulty,
    Urgency,
    Progress,
    HelpWanted,
}

// DecompositionResult — what the Queen returns
pub struct DecompositionResult {
    pub tasks: Vec<DecomposedTask>,
    pub single_agent_recommended: bool,
    pub reasoning: String,
}

pub struct DecomposedTask {
    pub id: String,
    pub description: String,
    pub dependencies: Vec<String>,
    pub context_tags: Vec<String>,
    pub estimated_tokens: u32,
}

// SelectionExponents — tunable parameters
pub struct SelectionExponents {
    pub alpha: f64,  // affinity
    pub beta: f64,   // urgency
    pub gamma: f64,  // difficulty
    pub delta: f64,  // failure
    pub zeta: f64,   // reward
}

// WorkerState — tracks what a worker is doing
pub struct WorkerState {
    pub id: String,
    pub current_task: Option<String>,
    pub recent_tags: Vec<String>,
    pub tasks_completed: u32,
    pub tokens_used: u64,
}
```

### 1.4 Config (`src/config.rs`)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HiveConfig {
    #[serde(default)]
    pub enabled: bool,                          // false by default
    #[serde(default = "default_min_workers")]
    pub min_workers: usize,                     // 1
    #[serde(default = "default_max_workers")]
    pub max_workers: usize,                     // 3
    #[serde(default = "default_swarm_threshold")]
    pub swarm_threshold_speedup: f64,           // 1.3
    #[serde(default = "default_queen_cost_ratio")]
    pub queen_cost_ratio_max: f64,              // 0.10
    #[serde(default = "default_budget_overhead")]
    pub budget_overhead_max: f64,               // 1.15
    #[serde(default)]
    pub pheromone: PheromoneConfig,
    #[serde(default)]
    pub selection: SelectionConfig,
    #[serde(default)]
    pub blocker: BlockerConfig,
}
```

---

## Phase 2: Blackboard (SQLite Task DAG)

### 2.1 File: `src/blackboard.rs`

**Public API:**

```rust
impl Blackboard {
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError>;
    pub async fn create_order(order: &HiveOrder) -> Result<(), _>;
    pub async fn create_tasks(tasks: &[HiveTask]) -> Result<(), _>;
    pub async fn claim_task(task_id: &str, worker_id: &str) -> Result<bool, _>;
    pub async fn complete_task(task_id: &str, result: &str, tokens: u32) -> Result<Vec<String>, _>;
    // ^ returns list of task IDs that became READY
    pub async fn fail_task(task_id: &str, error: &str) -> Result<HiveTaskStatus, _>;
    // ^ returns new status (Retry or Escalate based on retry_count)
    pub async fn get_ready_tasks(order_id: &str) -> Result<Vec<HiveTask>, _>;
    pub async fn get_task(task_id: &str) -> Result<Option<HiveTask>, _>;
    pub async fn get_order(order_id: &str) -> Result<Option<HiveOrder>, _>;
    pub async fn get_dependency_results(task_id: &str) -> Result<Vec<(String, String)>, _>;
    // ^ returns (task_id, result_summary) for all dependencies
    pub async fn is_order_complete(order_id: &str) -> Result<bool, _>;
    pub async fn get_order_results(order_id: &str) -> Result<Vec<HiveTask>, _>;
    // ^ returns all tasks in topological order
}
```

**Tests (10 minimum):**
1. Create order + tasks
2. Dependency resolution: completing t1 makes t2 ready
3. Atomic claim: two claims on same task, only one succeeds
4. Complete task updates tokens and result_summary
5. Fail task increments retry_count
6. Fail task with max retries → Escalate
7. Get ready tasks returns only Ready status
8. Get dependency results returns correct summaries
9. Order completion detection
10. Topological ordering of results

---

## Phase 3: Pheromone Field

### 3.1 File: `src/pheromone.rs`

**Public API:**

```rust
impl PheromoneField {
    pub async fn new(database_url: &str) -> Result<Self, Temm1eError>;
    pub async fn emit(&self, signal: PheromoneSignal) -> Result<(), _>;
    pub async fn read_total(&self, signal_type: SignalType, target: &str) -> Result<f64, _>;
    // ^ returns Σ intensities at current time
    pub async fn read_all(&self, target: &str) -> Result<HashMap<SignalType, f64>, _>;
    // ^ returns all signal totals for a target
    pub async fn gc(&self) -> Result<usize, _>;
    // ^ deletes expired signals, returns count deleted
    pub fn start_gc_loop(self: &Arc<Self>, interval_secs: u64);
    // ^ spawns background tokio task for periodic GC
}
```

**Intensity calculation (in Rust, not SQL):**
```rust
fn current_intensity(signal: &PheromoneSignal, now_ms: i64) -> f64 {
    let dt_secs = (now_ms - signal.created_at) as f64 / 1000.0;
    let value = signal.intensity * (-signal.decay_rate * dt_secs).exp();
    if signal.decay_rate < 0.0 {
        value.min(5.0)  // urgency cap
    } else {
        value
    }
}
```

**Tests (8 minimum):**
1. Emit and read single signal
2. Exponential decay over time
3. Superposition: 3 signals sum correctly
4. Urgency signal grows over time
5. Urgency capped at 5.0
6. GC removes expired signals (intensity < 0.01)
7. GC preserves active signals
8. Different signal types on same target are independent

---

## Phase 4: DAG Validation + Critical Path

### 4.1 File: `src/dag.rs`

**Public API:**

```rust
pub fn validate_dag(tasks: &[DecomposedTask]) -> Result<(), Temm1eError>;
// ^ Kahn's algorithm cycle detection

pub fn critical_path(tasks: &[DecomposedTask]) -> f64;
// ^ Returns estimated critical path duration (sum of estimated_tokens on longest path)

pub fn max_speedup(tasks: &[DecomposedTask]) -> f64;
// ^ total_estimated / critical_path — theoretical max parallelism benefit

pub fn topological_sort(tasks: &[DecomposedTask]) -> Result<Vec<String>, Temm1eError>;
// ^ Returns task IDs in topological order
```

**Tests (6 minimum):**
1. Valid DAG passes validation
2. Cyclic graph rejected
3. Self-referencing task rejected
4. Single task: speedup = 1.0
5. Fully parallel tasks: speedup = task_count
6. Serial chain: speedup = 1.0
7. Diamond DAG: correct critical path

---

## Phase 5: Selection Equation

### 5.1 File: `src/selection.rs`

**Public API:**

```rust
pub struct TaskSelector {
    exponents: SelectionExponents,
    tie_threshold: f64,
}

impl TaskSelector {
    pub fn new(config: &SelectionConfig) -> Self;

    pub async fn select_task(
        &self,
        worker: &WorkerState,
        ready_tasks: &[HiveTask],
        pheromones: &PheromoneField,
        total_tasks: usize,
        dependency_counts: &HashMap<String, usize>,  // task_id → number of dependents
    ) -> Option<String>;
    // ^ returns task_id of selected task, or None if no tasks available

    pub fn score(
        &self,
        affinity: f64,
        urgency: f64,
        difficulty: f64,
        failure: f64,
        reward: f64,
    ) -> f64;
    // ^ pure function, exposed for testing
}
```

**Affinity computation:**
```rust
fn tag_affinity(worker_tags: &[String], task_tags: &[String]) -> f64 {
    if worker_tags.is_empty() || task_tags.is_empty() {
        return 0.1;
    }
    let w: HashSet<&str> = worker_tags.iter().map(|s| s.as_str()).collect();
    let t: HashSet<&str> = task_tags.iter().map(|s| s.as_str()).collect();
    let intersection = w.intersection(&t).count();
    let union = w.union(&t).count();
    (intersection as f64 / union as f64).max(0.1)
}
```

**Tests (8 minimum):**
1. Score increases with affinity
2. Score increases with urgency
3. Score decreases with difficulty
4. Score decreases with failure
5. Score increases with downstream reward
6. Tie-breaking: scores within 5% → random selection
7. Zero ready tasks → None
8. Single ready task → always selected

---

## Phase 6: Queen Decomposition

### 6.1 File: `src/queen.rs`

**Public API:**

```rust
pub struct Queen {
    config: HiveConfig,
}

impl Queen {
    pub fn new(config: &HiveConfig) -> Self;

    pub fn should_decompose(message: &str) -> bool;
    // ^ Heuristic check: length, structure markers

    pub fn build_decomposition_prompt(message: &str) -> String;
    // ^ Returns the prompt for the LLM

    pub fn parse_decomposition(response: &str) -> Result<DecompositionResult, Temm1eError>;
    // ^ Parses LLM JSON response

    pub fn should_activate_swarm(
        decomposition: &DecompositionResult,
        queen_tokens: u64,
        estimated_single_cost: u64,
    ) -> bool;
    // ^ Checks activation threshold
}
```

**Decomposition prompt (exact text):**
```
You are a task decomposer for an AI agent runtime. Break the user's request into atomic subtasks.

RULES:
1. Each task must be completable by a single agent worker in one tool-use loop
2. Minimize dependencies between tasks — maximize parallelism
3. If the request is simple (1-2 steps), set single_agent_recommended: true
4. Include context_tags for each task (e.g., ["rust", "api", "database"])
5. Estimated tokens should be conservative (overestimate by 20%)
6. Task IDs must be sequential: t1, t2, t3, ...
7. Dependencies reference task IDs: ["t1", "t2"] means this task needs t1 and t2 to complete first

USER REQUEST:
{message}

Respond with ONLY valid JSON:
{
  "tasks": [
    {"id": "t1", "description": "...", "dependencies": [], "context_tags": ["..."], "estimated_tokens": 3000}
  ],
  "single_agent_recommended": false,
  "reasoning": "Brief explanation of decomposition strategy"
}
```

**Tests (6 minimum):**
1. should_decompose: short message → false
2. should_decompose: long structured message → true
3. parse_decomposition: valid JSON parsed correctly
4. parse_decomposition: malformed JSON → error
5. should_activate_swarm: low speedup → false
6. should_activate_swarm: high speedup, low queen cost → true

---

## Phase 7: Worker Execution Loop

### 7.1 File: `src/worker.rs`

**Public API:**

```rust
pub struct HiveWorker {
    pub id: String,
    pub state: WorkerState,
}

impl HiveWorker {
    pub fn new() -> Self;

    pub async fn run_loop(
        &mut self,
        order_id: &str,
        blackboard: &Blackboard,
        pheromones: &PheromoneField,
        selector: &TaskSelector,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        memory: Arc<dyn Memory>,
        config: &HiveConfig,
        cancel: CancellationToken,
    ) -> Result<(), Temm1eError>;
    // ^ Main loop: select → claim → execute → complete/fail → repeat

    async fn execute_task(
        &mut self,
        task: &HiveTask,
        dependency_results: &[(String, String)],
        provider: &dyn Provider,
        tools: &[Arc<dyn Tool>],
        memory: &dyn Memory,
    ) -> Result<TaskResult, Temm1eError>;
    // ^ Builds scoped context, runs agent loop, returns result
}

pub struct TaskResult {
    pub summary: String,
    pub tokens_used: u32,
    pub artifacts: Vec<String>,
    pub success: bool,
    pub error: Option<String>,
}
```

**Scoped context construction:**
```rust
fn build_scoped_context(
    task: &HiveTask,
    dependency_results: &[(String, String)],
    system_prompt: &str,
) -> Vec<ChatMessage> {
    let mut messages = vec![];

    // System prompt
    messages.push(ChatMessage::system(system_prompt));

    // Dependency results as context
    if !dependency_results.is_empty() {
        let ctx = dependency_results.iter()
            .map(|(id, result)| format!("## Result from task {id}:\n{result}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        messages.push(ChatMessage::system(format!(
            "Previous task results (your context):\n\n{ctx}"
        )));
    }

    // Task as user message
    messages.push(ChatMessage::user(&task.description));

    messages
}
```

---

## Phase 8: Hive Orchestrator (Public API)

### 8.1 File: `src/lib.rs`

**Public API — this is what main.rs calls:**

```rust
pub struct Hive {
    blackboard: Blackboard,
    pheromones: Arc<PheromoneField>,
    selector: TaskSelector,
    queen: Queen,
    config: HiveConfig,
}

impl Hive {
    pub async fn new(config: &HiveConfig, database_url: &str) -> Result<Self, Temm1eError>;

    /// Decide whether to use swarm mode for this message.
    /// Returns None if single-agent is better.
    pub async fn maybe_decompose(
        &self,
        message: &str,
        provider: &dyn Provider,
        model: &str,
    ) -> Result<Option<HiveOrder>, Temm1eError>;

    /// Execute an order using swarm workers.
    /// Returns the aggregated result text.
    pub async fn execute_order(
        &self,
        order: &HiveOrder,
        provider: Arc<dyn Provider>,
        tools: Vec<Arc<dyn Tool>>,
        memory: Arc<dyn Memory>,
        model: &str,
        cancel: CancellationToken,
    ) -> Result<SwarmResult, Temm1eError>;

    /// Get current swarm status (for dashboard/logging).
    pub async fn status(&self, order_id: &str) -> Result<SwarmStatus, Temm1eError>;
}

pub struct SwarmResult {
    pub text: String,                    // aggregated response
    pub total_tokens: u64,
    pub tasks_completed: usize,
    pub tasks_escalated: usize,
    pub wall_clock_ms: u64,
    pub workers_used: usize,
}

pub struct SwarmStatus {
    pub order_id: String,
    pub progress: f64,                   // 0.0 - 1.0
    pub tasks: Vec<HiveTask>,
    pub active_workers: usize,
}
```

---

## Phase 9: Integration into main.rs

### 9.1 Config addition (temm1e-core/types/config.rs)

Add to `Temm1eConfig`:
```rust
#[serde(default)]
pub hive: temm1e_hive::HiveConfig,
```

### 9.2 main.rs integration (in dispatcher)

```rust
// In the worker task, before calling agent.process_message():
if config.hive.enabled {
    let hive = Hive::new(&config.hive, &db_url).await?;
    if let Some(order) = hive.maybe_decompose(&msg.text, &provider, &model).await? {
        let result = hive.execute_order(
            &order, provider.clone(), tools.clone(), memory.clone(),
            &model, cancel.clone()
        ).await?;
        // Send result.text as OutboundMessage
        // Skip normal agent.process_message()
        return;
    }
}
// Fall through to normal single-agent processing
```

---

## Phase 10: Tests

### 10.1 Unit Test Count Target

| Module | Tests |
|--------|-------|
| types.rs | 5 (serde roundtrip, status transitions) |
| blackboard.rs | 10 (CRUD, claim, dependency resolution) |
| pheromone.rs | 8 (decay, superposition, GC, urgency) |
| dag.rs | 7 (cycle detection, critical path, sort) |
| selection.rs | 8 (score factors, tie-breaking) |
| queen.rs | 6 (heuristic, parsing, activation) |
| worker.rs | 4 (context building, lifecycle) |
| lib.rs | 3 (initialization, passthrough, integration) |
| **Total** | **51** |

### 10.2 Compilation Gate

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
```

ALL must pass. No exceptions.

---

## Phase 11: A/B Benchmark

### 11.1 Benchmark Script

File: `tems_lab/swarm/bench_ab.rs` (standalone binary or test)

Tasks to benchmark:
1. **Simple:** "What is the capital of France?" (should NOT activate swarm)
2. **3-step:** "Create a Rust function that reads a file, counts words, and writes stats to a new file"
3. **7-step:** "Build a REST API with: 1) database schema, 2) connection pool, 3) CRUD endpoints, 4) error handling, 5) input validation, 6) tests, 7) documentation"
4. **10-step:** "Refactor a module: 1) extract interfaces, 2) split into files, 3) add error types, 4) implement traits, 5) add logging, 6) write unit tests, 7) integration tests, 8) update imports, 9) update docs, 10) run clippy"

### 11.2 Metrics Collection

```rust
struct BenchmarkResult {
    task_name: String,
    mode: String,              // "single" or "swarm"
    run_number: u32,
    tokens_input: u64,
    tokens_output: u64,
    tokens_total: u64,
    cost_usd: f64,
    wall_clock_ms: u64,
    tasks_decomposed: usize,   // 0 for single
    tasks_parallel: usize,     // 0 for single
    quality_score: f64,        // 0.0 - 1.0
    swarm_activated: bool,
    error: Option<String>,
}
```

### 11.3 Budget Tracking

Running total across all benchmark runs. Stop if cumulative > $25 (leave $5 buffer).

```
Gemini 3.1 Flash Lite estimated pricing:
  Input:  $0.075 / 1M tokens
  Output: $0.30  / 1M tokens

Per benchmark run (~150K tokens avg):
  Cost ≈ $0.045

36 runs (12 tasks × 3 each): ~$1.62
Buffer for retries/debugging: ~$3.00
Total estimated: ~$5.00 of $30 budget
```

---

## Implementation Order

1. ✅ Design doc (DESIGN.md)
2. ✅ Implementation plan (this document)
3. 🔲 Create crate scaffold (Cargo.toml, lib.rs, mod declarations)
4. 🔲 Implement types.rs
5. 🔲 Implement config.rs
6. 🔲 Implement dag.rs + tests
7. 🔲 Implement blackboard.rs + tests
8. 🔲 Implement pheromone.rs + tests
9. 🔲 Implement selection.rs + tests
10. 🔲 Implement queen.rs + tests
11. 🔲 Implement worker.rs + tests
12. 🔲 Implement lib.rs (Hive orchestrator) + tests
13. 🔲 Compilation gate (check + clippy + fmt + test)
14. 🔲 Integration: config.rs addition
15. 🔲 Integration: main.rs hive path
16. 🔲 Full compilation gate
17. 🔲 A/B benchmark script
18. 🔲 Run benchmarks
19. 🔲 Final report
