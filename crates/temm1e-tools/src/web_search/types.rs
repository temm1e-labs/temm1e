//! Core types for the web_search tool.
//!
//! See `docs/web_search/IMPLEMENTATION_DETAILS.md` for the design rationale.

use serde::{Deserialize, Serialize};

// =====================================================================
// Constants — defaults and hard caps for output bounding.
// HARD caps cannot be exceeded by any agent param. Defaults can be
// overridden by config or per-call params, both clamped to HARD caps.
// =====================================================================

pub const DEFAULT_MAX_RESULTS: usize = 10;
pub const DEFAULT_MAX_TOTAL_CHARS: usize = 8_000; // ~2K tokens
pub const DEFAULT_MAX_SNIPPET_CHARS: usize = 200;

pub const HARD_MAX_RESULTS: usize = 30;
pub const HARD_MAX_TOTAL_CHARS: usize = 16_000; // ~4K tokens
pub const HARD_MAX_SNIPPET_CHARS: usize = 500;

pub const MIN_MAX_RESULTS: usize = 1;
pub const MIN_MAX_TOTAL_CHARS: usize = 1_000;
pub const MIN_MAX_SNIPPET_CHARS: usize = 50;

pub const DEFAULT_BACKEND_TIMEOUT_SECS: u64 = 8;
pub const DEFAULT_CACHE_TTL_SECS: u64 = 300;
pub const DEFAULT_CACHE_CAPACITY: usize = 256;

/// Per-backend HTTP body cap.
/// Bounds memory pressure during HTTP read, before any parsing.
///
/// **Why 512 KB, not 64 KB:** GitHub's `/search/repositories` returns ~5-8 KB
/// per repo with full metadata (owner, license, topics, URLs, dates). Even at
/// 20 results that's 100-160 KB. Brave can return similar. Cutting mid-response
/// at 64 KB produced `EOF while parsing a string` errors in self-test. 512 KB
/// covers realistic worst cases (50 results × 10 KB) with headroom, and still
/// bounds memory pressure for the Tokio runtime. Beyond 512 KB indicates an
/// API returning unexpectedly verbose data — we'd rather fail fast than
/// accumulate multi-MB strings per call.
pub const MAX_BACKEND_RESPONSE_BYTES: usize = 512 * 1024;

/// Multiplier from `max_results` to per-backend raw hit cap.
/// Each backend returns at most `max_results × this` hits before merge.
pub const PER_BACKEND_RAW_MULTIPLIER: usize = 2;

// =====================================================================
// Backend identifier
// =====================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendId {
    HackerNews,
    Wikipedia,
    Github,
    StackOverflow,
    Reddit,
    Marginalia,
    Arxiv,
    Pubmed,
    // Phase 2+
    Wikidata,
    DuckDuckGo,
    SearXng,
    // Phase 4 — paid
    Exa,
    Brave,
    Tavily,
    /// User-defined custom backend; the inner string is the user's chosen ID.
    /// Custom backends are matched by string ID, not enum variant.
    Custom(u32),
}

impl BackendId {
    /// Stable lower-snake-case name used in tool input/output and config.
    /// For Custom variants, returns "custom" as a generic label — the real
    /// id is carried separately in the CustomBackend wrapper.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::HackerNews => "hackernews",
            Self::Wikipedia => "wikipedia",
            Self::Github => "github",
            Self::StackOverflow => "stackoverflow",
            Self::Reddit => "reddit",
            Self::Marginalia => "marginalia",
            Self::Arxiv => "arxiv",
            Self::Pubmed => "pubmed",
            Self::Wikidata => "wikidata",
            Self::DuckDuckGo => "duckduckgo",
            Self::SearXng => "searxng",
            Self::Exa => "exa",
            Self::Brave => "brave",
            Self::Tavily => "tavily",
            Self::Custom(_) => "custom",
        }
    }

    /// Parse a backend name from a lowercase string.
    /// Custom backends are not matched here — they go through CustomRegistry.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "hackernews" | "hn" => Some(Self::HackerNews),
            "wikipedia" | "wiki" => Some(Self::Wikipedia),
            "github" | "gh" => Some(Self::Github),
            "stackoverflow" | "stack" | "so" => Some(Self::StackOverflow),
            "reddit" => Some(Self::Reddit),
            "marginalia" => Some(Self::Marginalia),
            "arxiv" => Some(Self::Arxiv),
            "pubmed" => Some(Self::Pubmed),
            "wikidata" => Some(Self::Wikidata),
            "duckduckgo" | "ddg" => Some(Self::DuckDuckGo),
            "searxng" => Some(Self::SearXng),
            "exa" => Some(Self::Exa),
            "brave" => Some(Self::Brave),
            "tavily" => Some(Self::Tavily),
            _ => None,
        }
    }
}

// =====================================================================
// Hit signal — backend-specific extras rendered in the snippet
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HitSignal {
    HnPoints {
        points: u32,
        comments: u32,
    },
    GithubStars {
        stars: u32,
        language: Option<String>,
    },
    StackOverflowScore {
        score: i32,
        answers: u32,
        accepted: bool,
    },
    RedditUpvotes {
        ups: i32,
        comments: u32,
        subreddit: String,
    },
    ArxivAuthors {
        authors: Vec<String>,
        primary_category: Option<String>,
    },
    PubmedAuthors {
        authors: Vec<String>,
        journal: String,
    },
    Wikipedia {
        description: Option<String>,
    },
    MarginaliaQuality {
        quality: f32,
    },
}

// =====================================================================
// SearchHit — normalized result across all backends
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub source: BackendId,
    /// Display name of the source. For custom backends, the user's id.
    pub source_name: String,
    pub published: Option<String>,
    /// Backend-native score in 0..=1 range. Used for merging.
    pub score: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<HitSignal>,
    /// Other backends that also returned this URL after dedup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_in: Vec<String>,
}

// =====================================================================
// Search request — what backends actually receive
// =====================================================================

#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub max_results: usize,
    pub max_total_chars: usize,
    pub max_snippet_chars: usize,
    pub time_range: TimeRange,
    pub category: Option<Category>,
    pub language: Option<String>,
    pub region: Option<String>,
    pub include_domains: Vec<String>,
    pub exclude_domains: Vec<String>,
    pub sort: SortOrder,
}

impl SearchRequest {
    /// Per-backend raw hit cap. Bounds memory pressure during dispatch.
    pub fn per_backend_raw_cap(&self) -> usize {
        self.max_results.saturating_mul(PER_BACKEND_RAW_MULTIPLIER)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeRange {
    Day,
    Week,
    Month,
    Year,
    All,
}

impl TimeRange {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "day" => Some(Self::Day),
            "week" => Some(Self::Week),
            "month" => Some(Self::Month),
            "year" => Some(Self::Year),
            "all" => Some(Self::All),
            _ => None,
        }
    }

    /// Cutoff in seconds before now. None for All.
    pub fn cutoff_secs(&self) -> Option<i64> {
        match self {
            Self::Day => Some(86_400),
            Self::Week => Some(604_800),
            Self::Month => Some(2_592_000),
            Self::Year => Some(31_536_000),
            Self::All => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Relevance,
    Date,
    Score,
}

impl SortOrder {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "relevance" => Some(Self::Relevance),
            "date" => Some(Self::Date),
            "score" => Some(Self::Score),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Company,
    ResearchPaper,
    News,
    PersonalSite,
    FinancialReport,
    People,
    Code,
}

impl Category {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "company" => Some(Self::Company),
            "research_paper" | "paper" => Some(Self::ResearchPaper),
            "news" => Some(Self::News),
            "personal_site" => Some(Self::PersonalSite),
            "financial_report" => Some(Self::FinancialReport),
            "people" => Some(Self::People),
            "code" => Some(Self::Code),
            _ => None,
        }
    }
}

// =====================================================================
// Raw input — what we parse from the tool call JSON
// =====================================================================

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawSearchInput {
    pub query: String,
    #[serde(default)]
    pub max_results: Option<usize>,
    #[serde(default)]
    pub max_total_chars: Option<usize>,
    #[serde(default)]
    pub max_snippet_chars: Option<usize>,
    #[serde(default)]
    pub backends: Option<Vec<String>>,
    #[serde(default)]
    pub time_range: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub region: Option<String>,
    #[serde(default)]
    pub include_domains: Option<Vec<String>>,
    #[serde(default)]
    pub exclude_domains: Option<Vec<String>>,
    #[serde(default)]
    pub sort: Option<String>,
}

// =====================================================================
// Resolved input — after clamping
// =====================================================================

#[derive(Debug, Clone)]
pub struct ResolvedInput {
    pub req: SearchRequest,
    /// String-based filter so it can include custom backend IDs.
    pub backends_filter: Option<Vec<String>>,
    pub clamps_applied: Vec<String>,
}

// =====================================================================
// Backend error type
// =====================================================================

#[derive(Debug, Clone)]
pub enum BackendError {
    Network(String),
    Http { status: u16, body: String },
    Parse(String),
    RateLimited { retry_after_ms: u64 },
    Timeout,
    Disabled,
}

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network: {e}"),
            Self::Http { status, body } => {
                let preview = if body.len() > 80 { &body[..80] } else { body };
                write!(f, "http {status}: {preview}")
            }
            Self::Parse(e) => write!(f, "parse: {e}"),
            Self::RateLimited { retry_after_ms } => {
                write!(f, "rate limit, retry in {retry_after_ms}ms")
            }
            Self::Timeout => write!(f, "timeout"),
            Self::Disabled => write!(f, "disabled"),
        }
    }
}

// =====================================================================
// Catalog — what the format footer reports for discoverability
// =====================================================================

#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub available: Vec<String>,
    pub disabled_with_hint: Vec<(String, String)>,
    pub custom: Vec<String>,
}

// =====================================================================
// Backend outcome — per-backend result of a dispatcher fan-out
// =====================================================================

#[derive(Debug, Clone)]
pub enum BackendOutcome {
    Ok {
        id: BackendId,
        name: String,
        hits: Vec<SearchHit>,
        latency_ms: u128,
    },
    Failed {
        id: BackendId,
        name: String,
        error: String,
    },
    Skipped {
        id: BackendId,
        name: String,
        reason: String,
    },
    Timeout {
        id: BackendId,
        name: String,
    },
}

impl BackendOutcome {
    pub fn name(&self) -> &str {
        match self {
            Self::Ok { name, .. }
            | Self::Failed { name, .. }
            | Self::Skipped { name, .. }
            | Self::Timeout { name, .. } => name,
        }
    }
}

// =====================================================================
// DispatcherOutput — full result of a search call
// =====================================================================

#[derive(Debug, Clone)]
pub struct DispatcherOutput {
    pub query: String,
    pub req: SearchRequest,
    pub hits: Vec<SearchHit>,
    pub total_candidates_before_truncation: usize,
    pub backends_succeeded: Vec<String>,
    pub backends_failed: Vec<(String, String)>,
    pub backends_skipped: Vec<(String, String)>,
    pub catalog: Catalog,
    pub clamps_applied: Vec<String>,
    /// True if input parsing or resolve() rejected the request entirely.
    pub input_error: Option<String>,
}

impl DispatcherOutput {
    pub fn input_error(query: String, req: SearchRequest, msg: String) -> Self {
        Self {
            query,
            req,
            hits: vec![],
            total_candidates_before_truncation: 0,
            backends_succeeded: vec![],
            backends_failed: vec![],
            backends_skipped: vec![],
            catalog: Catalog::default(),
            clamps_applied: vec![],
            input_error: Some(msg),
        }
    }
}
