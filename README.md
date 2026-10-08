# SQLLM

**Your SQL client, with an agent inside.**

*An Oracle-first, AI-native desktop workspace for developers.*

🌐 [sqllm.dev](https://sqllm.dev/) · ✉️ [founder@sqllm.dev](mailto:founder@sqllm.dev) · Working prototype · Windows · Oracle 11g compatible

SQLLM combines the database tools developers already need with AI that understands schemas, queries, errors, execution plans and PL/SQL relationships. It works the way Toad users already work. It can also make sense of large legacy PL/SQL codebases using **small local models that keep code inside the corporate network**.

| Result grid | AI panel | Execution plan |
|---|---|---|
| ![grid](docs/images/grid.png) | ![ai](docs/images/ai.png) | ![plan](docs/images/plan.png) |

<sub>Screenshots are from the real app connected to Oracle 11g XE. AI responses come from a test model.</sub>

> 한국어 문서: [README.ko.md](README.ko.md)

---

## The problem

Banks, insurers, manufacturers, public agencies and telcos still run their core business on Oracle. Decades of business logic live in PL/SQL packages that few people still understand. These teams are badly served by current tools:

- **Legacy tooling hasn't caught up.** Classic Oracle IDEs are powerful but were designed before AI. The new AI-first editors target cloud databases and modern stacks, not an Oracle 11g instance behind a firewall.
- **Cloud AI is often off-limits.** Source code and schemas in regulated industries usually can't be sent to an external API. Policy blocks frontier models, and the small models that *are* allowed fall apart on large, messy PL/SQL.
- **Knowledge is walking out the door.** The engineers who wrote the system are retiring. Modernization and migration projects start with months of manual archaeology: *what does this package do, which tables does it write, what breaks if we change this column?*
- **AI that runs SQL is a non-starter.** Database teams won't adopt an assistant that can execute statements against production.

## The solution: beyond text-to-SQL

Text-to-SQL starts with a prompt box and ends with generated syntax. The developer still has to rebuild the database context by hand. **Context is what turns a model into a database teammate.** SQLLM is a working environment that brings the schema, server version, active SQL, Oracle error, execution plan and PL/SQL relationships into the conversation.

It combines three things that today come from separate tools or don't exist yet:

1. **A fast, reliable Oracle IDE.** Editor, virtual-scrolling result grid, execution plans, a PL/SQL debugger, a session and lock monitor, and in-grid editing. It supports old servers (11g) and the realities of corporate networks.
2. **A model-agnostic AI copilot.** Local models (Ollama, LM Studio, vLLM), any OpenAI-compatible endpoint, or Anthropic's Claude, chosen per environment. AI answers are suggestions in the editor. **No code path executes AI-generated SQL.**
3. **A legacy-code intelligence engine.** It breaks PL/SQL into chunks a 1–8B model can handle, analyzes each one, and stitches the results into call graphs, table CRUD matrices, cursor-to-DML data flows and impact analysis. Other AI agents (Claude Desktop, Claude Code, Cursor) can read the results through a built-in MCP server.

## Why now

- **Small models got good enough** to run on a workstation or an on-prem GPU box. Most tools assume a frontier model, though. SQLLM is designed around the constraints of small models.
- **MCP has become the standard way** for AI agents to use tools. An Oracle-aware, read-only MCP server lets the agents enterprises already use understand their databases safely.
- **Modernization budgets are growing** as legacy experts retire and cloud migrations move forward. Understanding existing PL/SQL is the first and most expensive step.

---

## Product: one workspace to query, understand, debug and operate

| | |
|---|---|
| **Schema-aware editing** | Fast, local completion for tables, columns, joins, CTEs, packages and procedures |
| **AI with real context** | Generate, explain, fix and optimize using the relevant schema, error and execution plan |
| **PL/SQL intelligence** | Trace calls, CRUD, impact, long cursors and data flow across legacy packages |
| **Agent-ready MCP** | Give Claude and compatible agents read-only access to allow-listed metadata and saved analysis |
| **Database operations** | Inspect plans, sessions, locks, transactions and paged results without leaving the workspace |
| **Local or frontier models** | Choose Ollama, LM Studio, vLLM, compatible endpoints or Anthropic for each environment |

### 1. Human in control: AI proposes, you review and run

- In the app, AI **only proposes** SQL. A human moves it into the editor, reads it and runs it. No code path executes an AI answer.
- The MCP server has **no tool that takes arbitrary SQL**. It serves dictionary metadata (objects, table structure, DDL) and analysis results, never table data. The test `no_tool_executes_sql` locks the tool list in place.
- Query results and passwords are never sent to a model. External providers require explicit opt-in on first use, and the CLI requires `--allow-remote`.
- Read-only profiles are enforced twice: by the app's own statement classifier and by the server's `SET TRANSACTION READ ONLY`.
- Credentials never touch config files. They live in the OS credential store (Windows Credential Manager or macOS Keychain), which the app, the MCP server and the CLI all share.

### 2. Built for enterprise reality

- **One dedicated thread per connection** for OCI calls. A slow query never freezes the UI, and a stuck tab never affects another.
- **Cancellation that works behind firewalls.** When NAT drops the cancel signal, SQLLM abandons the session and reconnects after 5 seconds. The UI frees up immediately.
- **Oracle 11g and later**, Instant Client 19, CP949 or UTF-8 file encodings, and offline installation with WebView2 bundled.
- **Guard rails.** UPDATE/DELETE without WHERE, DROP and TRUNCATE require confirmation. Uncommitted transactions are flagged. Every edit made in the grid is shown as SQL and confirmed before it runs.

### 3. Fast

Rust and Tauri 2. The installer is small, memory use is low, and startup is fast.

| Measured on Oracle 11g XE (release build) | |
|---|---|
| First page of results (500 rows) | 3–5 ms |
| Fetching 100,000 rows | < 200 ms |
| Context-aware autocomplete, per keystroke | 0.002–0.15 ms |

Autocomplete parses the statement under the cursor and suggests only what fits that clause. It suggests FK-based join conditions, columns of CTEs and inline views, and correlated subquery scopes. It uses no LLM and makes no database round trip.

### 4. Legacy PL/SQL intelligence

The hardest part of any Oracle modernization is understanding what is already there. SQLLM's analysis pipeline is built so that **a small, on-prem model gives useful answers and the structural results stay correct no matter what the model says.**

| Stage | What happens | Uses a model? |
|---|---|---|
| Chunking | Splits subprograms at statement boundaries (never inside IF or LOOP) and long SQL at clause boundaries (CTE → clauses → comma/AND), with context attached to each chunk | No |
| Static facts | Table CRUD, calls, sequences, dynamic SQL, COMMIT/ROLLBACK, swallowed exceptions, DB links, complexity | No |
| Statements and cursors | Every SQL statement, plus the DML that each cursor's data feeds into | No |
| Insight | Summary, steps, business rules and risks (with line numbers) for each chunk | Yes, one call per chunk |
| Integration | Call graph, CRUD matrix, entry points, cycles, dead code, COMMIT locations, table → affected entry points | No |

- **Correct by construction.** Call relations and CRUD come from the source, not from the model. A small model that invents a table name cannot corrupt the graph. Running with no model at all (`--static`) still produces the full integrated analysis.
- **Resumable and incremental.** Results are cached per chunk, keyed by chunk hash, model and prompt version. Edit the source, and only the changed chunks are re-analyzed.
- **Measurable.** `eval` scores model output on a customer's own code: JSON failures, *ungrounded names* (identifiers that don't appear in the chunk), risks without line numbers, latency and tokens. It also recommends a chunk size. Customers can pick a model based on evidence instead of vendor claims.
- **Open output.** Everything is JSON plus a Markdown report with Mermaid graphs, ready for jq, scripts or documentation pipelines.

### 5. An Oracle gateway for AI agents (MCP)

| Tool | Returns |
|---|---|
| `list_connections`, `list_objects`, `describe_table`, `get_ddl` | Dictionary metadata, never table data |
| `analysis_overview`, `analysis_unit` | Package purpose, business rules, CRUD by subprogram, cursor flows |
| `table_usage` | Who reads and writes a table, data lineage, affected entry points |
| `subprogram_relations`, `analysis_findings` | Callers and callees, reachable COMMITs, swallowed exceptions, cycles |

For example, ask Claude Desktop *"If we change a column in ORDERS, which batch jobs are affected?"* It answers from `table_usage` without opening a database session.

---

## How we're different

| | Classic Oracle IDEs | General AI coding assistants | **SQLLM** |
|---|---|---|---|
| Deep Oracle support (11g+, OCI, PL/SQL debugging) | ✅ | ❌ | ✅ |
| AI copilot | Limited or add-on | ✅ | ✅ |
| Runs fully on-prem with small local models | — | Rarely | ✅ Designed for it |
| Understands large legacy PL/SQL codebases | ❌ | Limited by context window | ✅ Chunk → facts → integration |
| Guarantees that AI cannot execute SQL | — | ❌ | ✅ Enforced by architecture and tests |
| Exposes the database to other AI agents (MCP) | ❌ | — | ✅ Read-only |

**Defensibility.** The core assets are hard to replicate quickly. They are a precise PL/SQL lexer and structural analyzer, the chunking strategy tuned for small models, static extraction of cursor-to-DML lineage, and an evaluation harness that turns model choice into a measurable decision. Every customer deployment adds real-world PL/SQL patterns that sharpen all of them.

## Who it's for

- **Primary users:** Oracle DBAs, PL/SQL developers and data engineers in regulated enterprises such as finance, insurance, public sector, manufacturing and telecom.
- **Buyers:** Heads of IT operations and application maintenance, and modernization or migration program owners.
- **Partners:** System integrators running legacy assessment and migration projects, where SQLLM's analysis replaces weeks of manual code review.

## Business model (proposed)

| Tier | For | Includes |
|---|---|---|
| **Developer** | Individual engineers | IDE, AI copilot (bring your own model), autocomplete, debugger |
| **Team** | Database and application teams | Shared profiles, MCP server, session monitor, central policy for AI providers |
| **Enterprise** | Organizations with legacy estates | Bulk PL/SQL analysis, impact and lineage reports, model evaluation, on-prem support, SSO and audit |
| **Assessment** | SIs and migration programs | Fixed-scope legacy analysis engagements built on the analysis engine |

## Status: building in public

SQLLM is an active, working prototype. It starts with the Oracle environments that generic AI coding tools often overlook. Everything described above is implemented and tested against a real Oracle 11g database:

- Desktop IDE: editor, grid with sorting, filtering and editing, execution plans, PL/SQL debugger (`DBMS_DEBUG`), session and lock monitor
- AI copilot with local and frontier providers, plus the MCP server with 10 read-only tools
- PL/SQL analysis engine with an app UI, an overnight batch CLI and a model-quality evaluation harness
- OS credential store integration, and a Windows installer built in CI

**Next milestones**

1. **Pilot with real local models** on customer PL/SQL, tuning prompts and chunk sizes with `eval`
2. **Dependency cross-check** against `ALL_DEPENDENCIES` to resolve synonyms and dynamic SQL
3. **Impact analysis UI and column-level lineage** (table/column → readers, writers, flows, entry points)
4. **SQL tuning assistant**: actual plans (`DISPLAY_CURSOR`), top SQL, bind capture
5. **Generated documentation** (HTML, Markdown, YAML) for packages and systems
6. **Schema compare and DDL export**
7. **Scale validation** at tens of thousands of tables and millions of lines of source

## Contact

Database work deserves an agent that understands the whole system. For investment, pilots or partnerships, contact **[founder@sqllm.dev](mailto:founder@sqllm.dev)** or visit **[sqllm.dev](https://sqllm.dev/)**.

---

## Getting started

### Install (Windows)

1. Install **Oracle Instant Client 19 (64-bit)** and add it to `PATH`. It is required for 11g servers and may already be present on machines that have Toad or SQL Developer.
2. Run `SQLStudio_x.y.z_x64-setup.exe`. It needs no internet connection because WebView2 is bundled.
3. Add a connection profile on first launch.

The configuration lives at `%APPDATA%\sqlstudio\config.toml`; see [`config.example.toml`](config.example.toml). Passwords and API keys are never written there.

> The installer, binaries and config paths still use the working name `sqlstudio`.

### Build from source

```bash
# Rust 1.80+, Node 20+
cargo build --release -p sqls-mcp -p sqls-analyze   # MCP server and analysis CLI (bundled into the app)
cd app && npm ci && npx tauri build                  # desktop app and installer
```

On Linux, install `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libssl-dev` first.

### Test

```bash
cargo test --workspace          # no database required
cd app && npx tsc --noEmit      # frontend type check

# Oracle 11g integration tests
docker run -d --name ora11 -e ORACLE_PASSWORD=oracle gvenzl/oracle-xe:11-slim
SQLS_TEST_DSN=system/oracle@<ip>:1521/XE cargo test --workspace -- --ignored --test-threads=1
```

### Architecture

```
crates/
  sqls-core/     Oracle sessions (thread per connection), SQL parsing, clause-aware autocomplete, dictionary, config
  sqls-llm/      LLM providers (Ollama / OpenAI-compatible / Anthropic), streaming, prompts
  sqls-mcp/      MCP server — dictionary and analysis reads only, no SQL execution
  sqls-analyze/  PL/SQL analysis — lexer, chunking, static facts, per-chunk model analysis, integration
app/
  src-tauri/     Tauri backend (DB, AI, file commands)
  src/           UI — CodeMirror editor, virtual-scrolling grid, AI panel
```

Full usage reference (keyboard shortcuts, autocomplete rules, debugger, analysis output format, MCP setup) is in the Korean README, [README.ko.md](README.ko.md). Design details are in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).
