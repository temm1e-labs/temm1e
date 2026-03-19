# λ-Memory Effectiveness Report

> Scoring recall accuracy across 50 recall questions (turns 51-100).
> **Author:** TEMM1E's Lab
> **Scoring:** CORRECT=1.0 | PARTIAL=0.5 | VAGUE=0.25 | WRONG=0.0 | HALLUCINATED=-0.5

---

## Results

| Metric | λ-Memory | Current Memory | Naive Summary |
|--------|----------|----------------|---------------|
| **Score** | **33.5/50** | **37.5/50** | **24.2/50** |
| **Accuracy %** | **67.0%** | **75.0%** | **48.5%** |
| Correct (1.0) | 26 | 30 | 17 |
| Partial (0.5) | 20 | 20 | 20 |
| Wrong (0.0) | 4 | 0 | 13 |
| Hallucinated (-0.5) | 0 | 0 | 0 |

## Efficiency: Score per Token

| Metric | λ-Memory | Current Memory | Naive Summary |
|--------|----------|----------------|---------------|
| Total tokens | 172,984 | 76,821 | 96,983 |
| Recall score | 33.5 | 37.5 | 24.2 |
| **Score per 1K tokens** | **0.194** | **0.488** | **0.250** |

## Per-Question Breakdown

### λ-Memory

| Turn | Score | Reason | Response |
|------|-------|--------|----------|
| T51 | 1.0 | correct | You prefer `snake_case` for variable names. I will remember  |
| T52 | 1.0 | correct | You've decided to use the `thiserror` crate for all your err |
| T53 | 1.0 | correct | No, you should never use `unwrap` in production code. Always |
| T54 | 1.0 | correct | You chose to use the `tracing` crate for logging, not the `l |
| T55 | 1.0 | correct | You prefer a blue-green deployment strategy. This approach m |
| T56 | 1.0 | correct | Ideally, database migrations should be reversible. This allo |
| T57 | 0.25 | vague (related but missing key terms) | While you haven't explicitly stated specific linting rules,  |
| T58 | 0.0 | amnesia (claimed not discussed) | You haven't specified a particular database timeout value in |
| T59 | 1.0 | correct | You haven't explicitly stated a preference between compositi |
| T60 | 0.25 | vague (related but missing key terms) | Rate limiting is best implemented as middleware in your Axum |
| T61 | 0.0 | amnesia (claimed not discussed) | You haven't specified a particular header that *must* be inc |
| T62 | 1.0 | correct | Given your choice of the `tracing` crate, you should use it  |
| T63 | 1.0 | correct | You've decided to use SQLite for development and Postgres fo |
| T64 | 0.0 | amnesia (claimed not discussed) | You haven't specified a maximum number of database connectio |
| T65 | 1.0 | correct | You chose `sqlx` for compile-time query checking. This ensur |
| T66 | 1.0 | correct | When dealing with file paths in your application, you must b |
| T67 | 1.0 | correct | You've decided to use JWTs with short expiry times, along wi |
| T68 | 0.25 | vague (related but missing key terms) | You haven't explicitly defined separate CORS policies for de |
| T69 | 1.0 | correct | No, you should never expose internal errors directly to API  |
| T70 | 0.25 | vague (related but missing key terms) | Okay, here's a list of all the explicit "remember" requests  |
| T71 | 0.5 | partial (mixed signals) | You chose Axum over Actix-web for your web framework. I will |
| T72 | 1.0 | correct | You prefer `match` over `if-let` for complex pattern matchin |
| T73 | 0.25 | vague (related but missing key terms) | While you haven't explicitly stated a requirement for public |
| T74 | 1.0 | correct | You've expressed frustration with verbose error handling. Th |
| T75 | 0.5 | subjective (answered) | The most critical architectural decision is to avoid exposin |
| T76 | 0.5 | subjective (answered) | You made 4 explicit "remember" requests. However, I've also  |
| T77 | 0.25 | vague (related but missing key terms) | Here's a summary of your preferences, based on our conversat |
| T78 | 0.25 | vague (related but missing key terms) | Based on our conversation, you've chosen the following frame |
| T79 | 0.5 | subjective (answered) | Based on my λ-Memory and the recency of our interactions, I' |
| T80 | 0.5 | subjective (answered) | If I could only retain one preference, it would be the decis |
| T81 | 0.5 | partial (mixed signals) | The first thing you said was, "Should internal errors be exp |
| T82 | 1.0 | correct | You've chosen SQLite for development and Postgres for produc |
| T83 | 1.0 | correct | Your security requirements include sanitizing user-provided  |
| T84 | 1.0 | correct | Your API design decisions include avoiding exposing internal |
| T85 | 0.0 | wrong | We discussed a few patterns: |
| T86 | 1.0 | correct | No, you haven't explicitly changed your mind on any decision |
| T87 | 0.25 | vague (related but missing key terms) | Okay, here's a comprehensive summary of your project, based  |
| T88 | 1.0 | correct | We haven't explicitly discussed your testing philosophy. How |
| T89 | 0.5 | subjective (answered) | Given your focus on security and API design, you should firs |
| T90 | 0.5 | subjective (answered) | The single most important thing to remember is to **never ex |
| T91 | 0.5 | subjective (answered) | Based on the summary, we covered approximately 12 distinct t |
| T92 | 1.0 | correct | Your infrastructure decisions include using SQLite for devel |
| T93 | 1.0 | correct | For error handling, you've chosen to use the `thiserror` cra |
| T94 | 1.0 | correct | We haven't explicitly discussed your middleware approach. Ho |
| T95 | 1.0 | correct | Given your choice of Axum and `sqlx`, it's clear you're buil |
| T96 | 0.25 | vague (related but missing key terms) | We haven't specifically discussed memory leaks, but given yo |
| T97 | 0.5 | subjective (answered) | Okay, I'll grade myself on memory recall based on this conve |
| T98 | 0.25 | vague (related but missing key terms) | The benchmark start time was recorded, but I don't have the  |
| T99 | 1.0 | greeting/farewell | You're welcome! I appreciate the feedback and will use it to |
| T100 | 1.0 | greeting/farewell | Goodbye! Feel free to reach out if you have any more questio |

### Current

| Turn | Score | Reason | Response |
|------|-------|--------|----------|
| T51 | 1.0 | correct | You prefer `snake_case` for variable names. |
| T52 | 1.0 | correct | You chose `thiserror` for all error types. I will remember t |
| T53 | 1.0 | correct | No, you should never use `unwrap` in production. Always use  |
| T54 | 1.0 | correct | You chose to use structured logging with `tracing` spans for |
| T55 | 1.0 | correct | Your deployment strategy is always blue-green deployments. |
| T56 | 1.0 | correct | Yes, your DB migrations must be reversible. |
| T57 | 1.0 | correct | Your CI runs `clippy` with `-D warnings`. |
| T58 | 1.0 | correct | You specified a 5-second timeout for all database operations |
| T59 | 1.0 | correct | You prefer composition over inheritance. |
| T60 | 1.0 | correct | You prefer rate limiting at the gateway level. |
| T61 | 1.0 | correct | All your API responses need a `request-id` header. |
| T62 | 1.0 | correct | You want structured logging with `tracing` spans for your da |
| T63 | 1.0 | correct | You prefer SQLite for development and Postgres for productio |
| T64 | 1.0 | correct | You want a maximum of 20 database connections in the pool. |
| T65 | 1.0 | correct | You chose `sqlx` for compile-time query checking. |
| T66 | 1.0 | correct | Remember: sanitize file paths to prevent directory traversal |
| T67 | 1.0 | correct | You're using JWTs with short expiry + refresh tokens for aut |
| T68 | 1.0 | correct | Okay, I'll remember that you want permissive CORS in develop |
| T69 | 1.0 | correct | Understood. I will remember to never expose internal errors  |
| T70 | 0.25 | vague (related but missing key terms) | Okay, here's a list of all your explicit remember requests: |
| T71 | 0.5 | partial (mixed signals) | You chose `axum` over `actix-web`. |
| T72 | 1.0 | correct | Okay, I'll remember that you prefer `match` over `if-let` fo |
| T73 | 1.0 | correct | Your public types must implement `Debug`, `Clone`, and `Seri |
| T74 | 1.0 | correct | I understand. Since you're frustrated with verbose error han |
| T75 | 0.5 | subjective (answered) | The most critical architectural decision is to use hexagonal |
| T76 | 0.5 | subjective (answered) | You made 12 explicit remember requests. |
| T77 | 0.25 | vague (related but missing key terms) | Okay, here's a summary of all your preferences: |
| T78 | 0.25 | vague (related but missing key terms) | You picked the following frameworks and libraries: |
| T79 | 0.5 | subjective (answered) | I'd rate my confidence in recalling your preferences a 9/10. |
| T80 | 0.5 | subjective (answered) | If I could only keep one preference, it would be to never ex |
| T81 | 0.5 | partial (mixed signals) | The first thing you said was "What error handling library di |
| T82 | 0.25 | vague (related but missing key terms) | Okay, here's a summary of your database configuration: |
| T83 | 0.25 | vague (related but missing key terms) | Here's a summary of your security requirements: |
| T84 | 0.25 | vague (related but missing key terms) | Here's a summary of your API design decisions: |
| T85 | 1.0 | correct | We discussed design patterns for Rust ownership, including u |
| T86 | 1.0 | correct | No, you haven't changed your mind on any decisions so far. |
| T87 | 0.25 | vague (related but missing key terms) | Okay, here's a comprehensive summary of your project: |
| T88 | 1.0 | correct | Your testing philosophy includes using SQLite for developmen |
| T89 | 0.5 | subjective (answered) | You should document the API endpoints first. |
| T90 | 0.5 | subjective (answered) | The single most important thing to remember is to never expo |
| T91 | 0.5 | subjective (answered) | We covered approximately 12 distinct topics. |
| T92 | 0.25 | vague (related but missing key terms) | You haven't made specific infrastructure decisions yet. |
| T93 | 0.25 | vague (related but missing key terms) | Here's a full summary of your error handling approach: |
| T94 | 1.0 | correct | You plan to use Tower middleware for composing modular, reus |
| T95 | 1.0 | correct | You prefer async Rust for handling concurrency in I/O-bound  |
| T96 | 1.0 | correct | To prevent memory leaks with `Arc`, use `Weak` pointers to b |
| T97 | 0.5 | subjective (answered) | I'd grade myself an A on memory recall for our conversation. |
| T98 | 0.25 | vague (related but missing key terms) | The benchmark start time wasn't mentioned in our conversatio |
| T99 | 1.0 | greeting/farewell | You're welcome! I'm glad I could assist you. Feel free to re |
| T100 | 1.0 | greeting/farewell | Goodbye! |

### Naive

| Turn | Score | Reason | Response |
|------|-------|--------|----------|
| T51 | 0.0 | amnesia (claimed not discussed) | You haven't specified a variable naming convention preferenc |
| T52 | 0.0 | amnesia (claimed not discussed) | You haven't chosen a specific error handling library yet. |
| T53 | 1.0 | correct | No, you should not use `unwrap` in production code. `unwrap` |
| T54 | 0.25 | vague (related but missing key terms) | You haven't selected a logging framework in our conversation |
| T55 | 0.0 | amnesia (claimed not discussed) | You haven't specified a deployment strategy during our conve |
| T56 | 0.5 | partial (mixed signals) | You haven't specified whether your database migrations shoul |
| T57 | 0.0 | amnesia (claimed not discussed) | You haven't specified any CI linting requirements. |
| T58 | 0.0 | amnesia (claimed not discussed) | You haven't specified a specific database timeout value. How |
| T59 | 1.0 | correct | You haven't explicitly stated a preference between compositi |
| T60 | 0.0 | amnesia (claimed not discussed) | You haven't specified where rate limiting should occur in yo |
| T61 | 0.0 | amnesia (claimed not discussed) | I am sorry, but this was not covered in our conversation. |
| T62 | 0.25 | vague (related but missing key terms) | For logging database operations, consider logging SQL querie |
| T63 | 1.0 | correct | Using separate databases for development and production is a |
| T64 | 0.25 | vague (related but missing key terms) | A reasonable starting point for the maximum number of databa |
| T65 | 0.0 | amnesia (claimed not discussed) | You haven't chosen a specific query library yet. |
| T66 | 1.0 | correct | When handling file paths, sanitize and validate any user-pro |
| T67 | 1.0 | correct | For authentication, consider using JSON Web Tokens (JWTs). J |
| T68 | 1.0 | correct | For development, a permissive CORS policy (allowing requests |
| T69 | 1.0 | correct | No, internal errors should not be directly exposed to API co |
| T70 | 0.25 | vague (related but missing key terms) | Okay, here's a list of all your explicit remember requests f |
| T71 | 0.0 | amnesia (claimed not discussed) | You haven't specified a web framework. |
| T72 | 1.0 | correct | You have not specified a pattern matching style preference. |
| T73 | 0.0 | amnesia (claimed not discussed) | You haven't specified what public types must implement. |
| T74 | 0.25 | vague (related but missing key terms) | You did not specify your view on error handling verbosity. |
| T75 | 0.5 | subjective (answered) | You have not specified what the most critical architectural  |
| T76 | 0.5 | subjective (answered) | You made 7 explicit "remember requests". |
| T77 | 0.25 | vague (related but missing key terms) | Okay, here's a numbered list summarizing your preferences as |
| T78 | 0.25 | vague (related but missing key terms) | You have only picked a method for Authentication (JWT). You  |
| T79 | 0.5 | subjective (answered) | I'm very confident in recalling your stated preferences. I'd |
| T80 | 0.5 | subjective (answered) | If I could only keep one preference, it would be the one abo |
| T81 | 1.0 | correct | The first thing you said was "Logging SQL queries along with |
| T82 | 1.0 | correct | You prefer SQLite for development and either PostgreSQL or M |
| T83 | 0.25 | vague (related but missing key terms) | Okay, here's a summary of the security requirements we've di |
| T84 | 1.0 | correct | The main API design decision made so far is to avoid exposin |
| T85 | 0.0 | amnesia (claimed not discussed) | We have not discussed any specific design patterns. |
| T86 | 1.0 | correct | No, you have not changed your mind on any of the preferences |
| T87 | 0.25 | vague (related but missing key terms) | Okay, here's a complete summary of everything we've discusse |
| T88 | 1.0 | correct | You have not yet specified your testing philosophy. |
| T89 | 0.5 | subjective (answered) | Given your preferences, you should document your error handl |
| T90 | 0.5 | subjective (answered) | The single most important thing to remember is to avoid expo |
| T91 | 0.5 | subjective (answered) | We covered 7 distinct topics: Logging, Databases, DB Connect |
| T92 | 1.0 | correct | The infrastructure decisions you've made so far are choosing |
| T93 | 0.25 | vague (related but missing key terms) | Your error handling strategy focuses on security and user ex |
| T94 | 0.0 | amnesia (claimed not discussed) | We haven't discussed any specific middleware approaches yet. |
| T95 | 1.0 | correct | You haven't expressed a preference for asynchronous vs. sync |
| T96 | 0.0 | amnesia (claimed not discussed) | We haven't discussed memory leaks. |
| T97 | 0.5 | subjective (answered) | A. I believe I have accurately recalled and summarized all k |
| T98 | 0.25 | vague (related but missing key terms) | We did not discuss any benchmarking. |
| T99 | 1.0 | greeting/farewell | You're welcome! I'm ready for the next task. |
| T100 | 1.0 | greeting/farewell | Goodbye! Let me know if you need anything else in the future |

---
*Scored by TEMM1E's Lab automated rubric*
