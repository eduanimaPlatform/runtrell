# Runtrell

Runtrell — An embeddable Rust runtime for autonomous agents.
（自律Agentを製品に組み込むためのRust実行基盤）

Runtrell is developed and maintained by Eduanima as a product- and domain-independent library.
**Pre-release:** the API and storage schemas may change. There is no stable-release
or compatibility promise yet.

## Current scope

- `runtrell`: public SDK and driver, authorization dispatch, owner fencing,
  hierarchical budgets, run/operation contracts, a non-durable memory adapter,
  Workspace/Artifact stores, validation/finalization, and on-demand process execution.
  The `runtrell-exec-limit` binary applies process limits before launching isolation.
- `runtrell-postgres`: PostgreSQL control store, scheduler, budgets and execution
  ledger, with transactions, generation/lease fencing and recovery queries.
- `runtrell-protocol`: internal decision machine, message pairing and stream assembly.
  Its types are not the host-facing SDK contract.
- `runtrell-testkit`: fake adapters, failure scenarios and independent host examples.
- `xtask`: repository-local requirement trace and provenance checks; no runtime dependency.

Hosts implement `ModelPort`/`Tool`/`Policy`/`Validator`/`Finalizer` and supply a
`ControlStore`. Fake examples require no credentials, database, provider or sandbox.
`AgentRunStep::Done` is a judgment candidate: trusted validation and a fenced
Finalizer receipt establish Run success. External application, cancellation,
stopping, reclamation and settlement remain distinct records.

The ordinary driver continues `max_tokens` responses after settling usage,
discarding partial tool calls and reserving a newly sized request. Hosts set
`RunSpec.continuation` for the instruction and an optional continuation limit.
`RunSpec.compaction` enables automatic context summaries at a host threshold;
`ModelPort::compaction_limits` selects the summary model, which the host port
routes by request model. Original history and the active summary remain in the
same Run checkpoint and survive resume. See [driver contracts](docs/design/api.md).
Schema failures return JSON Pointer, cause and hint diagnostics for repair.
Host validators classify custom failures with `ValidationReceipt.repairable`.
Repair counts have no default limit; `RepairPolicy` supplies optional host limits.
Storage, authorization and evidence failures remain separate errors.

## PostgreSQL transaction API

| API | Stages in the host's `sqlx::Transaction<'_, Postgres>` |
|---|---|
| `PgStore::start_in` | Idempotent Run admission, queue permits, optional parent and existing account |
| `PgStore::create_account_in` / `create_account_with_policy_in` | New account with default or explicit policy |
| `PgStore::lock_account_in` | Tenant-checked account and ancestor locks; typed policy, limits and balances |
| `PgStore::terminate_in` | Fenced non-success terminal state |
| `PgStore::finalize_in` | Validated, fenced successful completion |

Hosts execute their own SQL in the same transaction and own commit/rollback.
Success from these methods means staged writes; only the outer commit makes them
durable. A failed call rolls back its savepoint. See [transaction scope](docs/design/postgres.md#host-owned-transaction).

## Optional provider adapters

`runtrell-providers` implements `ModelPort` with opt-in `openai` (Responses),
`gemini` (Developer API generateContent), and `openrouter` (chat completions)
features. The crate has no default features. Hosts supply endpoint, credentials,
model limits/capabilities, prices and Run/session binding; adapters perform one
bounded HTTP send without automatic retry or redirect. Usage preserves missing
counters and subset relationships. Saved continuation state is checked against
the owning Run/session before sending. Public OSS image adapters `OpenAiImages`
(`openai`) and `GeminiImages` (`gemini`) supply optional generation and reference
editing through the same standard image tools. Models are host supplied.
Remote lookup/idempotency are not claimed.
Optional [attachments](docs/design/attachments.md) support fixed external
URL/bytes and authorized Workspace/Artifact versions. `DocumentView` inspects PDF
page count and attaches selected pages; PNG/JPEG/WebP use ordinary image input
(host MIME allowlist applies). PDF visible annotations and form appearances stay
visible while active actions are removed. Tool references remain in history,
but only the latest tool exchange contributes binary attachments to each request.
Reopen fixed saved references when needed; compaction retains older references
as text. Oversized tool attachments return feedback asking for a narrower view.
Lookup does not fetch originals or issue URLs. See [provider contracts](docs/design/providers.md)
and [images](docs/design/images.md).

| Adapter | Image input | Generation | Reference editing | Mask editing |
|---|---|---|---|---|
| OpenAI / Gemini / OpenRouter ModelPort | Host-declared, authorized references (PNG default) | — | — | — |
| OpenAiImages (`openai`) | Authorized references | Host-declared | Host-declared | Unsupported |
| GeminiImages | Reference images | Host-declared | Host-declared | Unsupported |

The default image MIME is PNG. Hosts can opt into inspected still JPEG/WebP in
`ImageLimits.allowed_mime`. OpenAI forwards `size` when requested and
`output_format` (`png` by default, also `jpeg`/`webp` when enabled), always `n=1`,
without quality. GeminiImages supports PNG output and rejects size/other format
options. Image limits and authorization also apply to saved-result retrieval.

```sh
RUNTRELL_TEST_DATABASE_URL=postgres://postgres:runtrell@127.0.0.1:55433/postgres \
  cargo test -p runtrell-providers --locked --features openai,gemini,openrouter
```

## Standard tools chosen by the host

`standard_tools::ToolSet` builds ordinary `RegisteredTool` values for `Runtime.tools`.
It starts empty. Connecting a store or backend does not enable a tool. Use
`select`, `disable`, and `settings` per tool; `ToolPreset` only supplies a map of
selected tools and default settings, which later calls can overwrite.

| Tool | Common implementation | Host supplies |
|---|---|---|
| `files.list/read/write/delete/exact_replace` | `FileTool` | Workspace store, owned Access, storage limits |
| `files.search` | `FileTool` | Workspace store, Access, search bounds |
| `artifacts.read` | `FileTool` | Artifact store, Access, read bounds |
| `validate` | `ValidationTool` | TargetValidator, evidence store, pinned Run |
| `image.generate/edit` | `ImageTool` + `ToolSet::image` | ImagePort, stores, Access/grants, Policy, RunStore, Clock, reservation and limits |
| Large output envelope | `StoredOutputTool`, per selected tool | Artifact store, Access, inline size, retention/time |
| `execute` | `ExecutionTool` | Control store, Workspace/Artifact store, execution ledger/backend, Policy/Clock, executable/argument policies, enforced limits, reservation and preview size |

Hosts select and connect these implementations; custom `Tool` implementations
are only needed for application-specific operations. Business prompts, schemas,
quality criteria and authoritative application updates remain host-owned.
Image tools use one Runtime operation without starting a sandbox or Python.

```rust
use runtrell::{data::DataStore, standard_tools::{StandardTool, ToolSet}};
// Missing resources produce Vec<assembly::ConfigDiagnostic> during assembly.
let diagnostics = ToolSet::<DataStore>::default()
    .select(StandardTool::Execution).build().err().unwrap();
assert_eq!(diagnostics[0].code, "EXECUTION_BACKEND_MISSING");
```

Cargo features determine compiled capabilities (`python` is opt-in). Selection
chooses the catalog, `pin_catalog` filters definitions with the Run's Policy, and
Runtime checks Policy again for each call. None of these gates grants the others.
Python scripts require both the feature and an explicitly supplied `PythonRuntime`;
no interpreter or dependency is installed. See [API](docs/design/api.md),
[execution](docs/design/execution.md) and [Workspace](docs/design/workspace.md).

## Requirement acceptance

<!-- requirement-summary -->
分類は責任の配置、状態は要件全体の到達点であり独立した軸。公開ソースに存在することや試験件数は、134要件の全達成を意味しない。利用側の責任のfake接続試験は、実hostでの業務受入の証明ではない。

| 分類 | 検証済み | 実装済み・Linux未実行 | 設計済み | 未着手 | 合計 |
|---|---:|---:|---:|---:|---:|
| 共通基盤 | 44 | 2 | 63 | 0 | 109 |
| 公開の任意機能 | 1 | 1 | 11 | 0 | 13 |
| 利用側の責任 | 0 | 0 | 10 | 2 | 12 |
| 全体 | 45 | 3 | 84 | 2 | 134 |

公開範囲: 共通実行基盤と選択可能な公開tool/backend/adapter。業務接続・設定と業務ロジックはhost側の別責務。方針型と共通実装はRuntrell、具体値はhostが供給する。ファイル操作・検索・画像生成/編集・Python・標準モデル接続等の再利用可能な実装も任意機能として扱い、全Runへ強制しない。この分類はそれら全ての実装完了を主張しない。

試験件数: CIの各feature・suiteのlibtest結果を実行時の正本とする（同じ試験のfeature別実行を独立要件数へ加算しない）。Linux実隔離はintegration default 9件／python 10件と、test-only取得crash unit 3件（両feature）を必須とし、macOS実隔離は2件を名前とok結果で照合する。Hostedの成功はworkflow runの固定commit・runner・結果を確認して初めて記録する。変更後のHosted実行は未確認。本番構成・実provider接続・電源断・failover・負荷性能は未検証。
<!-- /requirement-summary -->

## Local validation

The pinned toolchain is Rust 1.98.0.

```sh
cargo build --workspace --locked
# One local test server (also safe to share with other test jobs).
# The password below is only an example for that local test container.
docker run -d --name runtrell-pg --shm-size=1g \
  --label org.runtrell.test-server=local \
  -e POSTGRES_PASSWORD=runtrell -p 127.0.0.1:55433:5432 postgres:18
export RUNTRELL_TEST_DATABASE_URL='postgres://postgres:runtrell@127.0.0.1:55433/postgres'
export CARGO_INCREMENTAL=0
cargo build -p runtrell-postgres --locked --example test_db_runner
# Point RIG_SNAPSHOT at a checkout of the pinned Rig commit listed below.
export RIG_SNAPSHOT=/path/to/rig-snapshot
# Formal local validation: 18 commands. Run the two test commands twice in a row
# when checking resource stability (20 executions, not 20 distinct commands).
cargo fmt --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo clippy --workspace --locked --all-targets --features runtrell/python,runtrell-testkit/python,runtrell-providers/openai,runtrell-providers/gemini,runtrell-providers/openrouter -- -D warnings
cargo test --workspace --locked
cargo test --workspace --locked --features runtrell/python,runtrell-testkit/python,runtrell-providers/openai,runtrell-providers/gemini,runtrell-providers/openrouter
cargo run -p runtrell-testkit --locked --example support_decision
cargo run -p runtrell-testkit --locked --example alternate_host
cargo run -p runtrell-postgres --locked --example chat_host
cargo run -p runtrell-testkit --locked --features runtrell/python,runtrell-testkit/python --example support_decision
cargo run -p runtrell-testkit --locked --features runtrell/python,runtrell-testkit/python --example alternate_host
cargo run -p runtrell-postgres --locked --features runtrell/python,runtrell-testkit/python --example chat_host
cargo check -p runtrell-providers --locked --no-default-features --features openai
cargo check -p runtrell-providers --locked --no-default-features --features gemini
cargo check -p runtrell-providers --locked --no-default-features --features openrouter
cargo run -p xtask --locked -- api-doc
cargo run -p xtask --locked -- trace
cargo run -p xtask --locked -- hashes
git diff --check
# Optional package inventory:
cargo package --workspace --list --allow-dirty
```

PostgreSQL tests require `RUNTRELL_TEST_DATABASE_URL`; they fail rather than skip
when it is absent. Point it only at a test server, using a role with CREATEDB.
Build the test-only `test_db_runner` example before running tests (and rebuild it
after editing its source). The repository's Unix Cargo target runner gives
**each binary/example invocation** a disposable
database. Its children inherit that database, so process-kill/restart tests and
the multi-stage `chat_host` keep their data until the parent suite has finished.
The runner then removes its database on success, test panic or ordinary failure;
`PgStore::close` retains its ordinary pool-only behavior. There is no automatic
schema deletion in the library. To run just the adapter tests, use
`cargo test -p runtrell-postgres` with the same variable. Run Cargo from the repo
root; overriding Cargo's target runner bypasses this lifecycle.

Cleanup is restricted to runner-created databases: the name, creation timestamp,
ownership comment, endpoint, database owner, process liveness and database advisory
lock must agree. A durable intent in `target/test-db-intents/` records the exact
name, endpoint and owner before creation, covering SIGKILL before the database
comment is written. Keep these small ownership files until cleanup completes.
The next invocation reclaims abandoned owned databases after SIGKILL, while
preserving another running suite. Unmarked databases without a matching local
intent and all schemas in the configured source database are untouched, including
older test leftovers. Ambiguous process liveness (including a reused PID) is
preserved for safety; retry after that process exits. Tests must join their
children; detached background jobs are outside this runner's lifecycle.
The runner does not create or delete containers/volumes and does not retain failed
test data as evidence. Keep failure logs in an explicitly chosen location with a
retention deadline if needed. A connection/configuration error fails closed.

Local configuration: PostgreSQL `postgres:18`, 1 GiB `/dev/shm`, server-default
PostgreSQL parallel settings and Cargo/libtest's default test parallelism. Run
workspace test commands sequentially when measuring residual resources.
The measured local settings are `max_connections=100`, `max_worker_processes=8`,
`max_parallel_workers=8` and `max_parallel_workers_per_gather=2`.
Hosted contracts keep the job-exclusive `postgres:18` service in
[ci.yml](.github/workflows/ci.yml), Docker's default shared memory (64 MiB), and
the same default parallel settings; no local 1 GiB requirement is imposed on CI.
The service is removed by the hosted job lifecycle; the target runner also
reclaims each suite database. On a shared local server, do not remove the server
container or its volume while another job uses it. For a server created by the
command above, first stop all consuming jobs and verify the image and ownership
label with `docker inspect runtrell-pg`; only then use
`docker stop runtrell-pg` and `docker rm -v runtrell-pg` to remove that owned server
and its anonymous volume. Do not apply this to an existing unlabelled/shared server.
Database cleanup is automatic
on the next `cargo test`/`cargo run`; no broad schema/container sweep is required.

The earlier incident reported 3,661 schemas and a shared-memory allocation
failure. Shared-memory exhaustion is supported by that error; schema count alone
does not establish equal resident shared-memory allocations. Container recreation
and increasing `/dev/shm` happened together, so their individual contributions
have not been isolated. Owned database cleanup prevents accumulation; increasing
shared memory alone does not.

For before/after measurements on the shared local server:

```sh
docker exec runtrell-pg psql -U postgres -c "SELECT count(*) FROM pg_namespace"
docker exec runtrell-pg psql -U postgres -c "SELECT datname FROM pg_database WHERE datname LIKE 'rt_suite_%'"
docker exec runtrell-pg psql -U postgres -c "SELECT datname, count(*) FROM pg_stat_activity WHERE backend_type='client backend' GROUP BY datname"
```

Compare owned databases/connections separately from other jobs' activity; the
maintenance database's schema count alone does not measure suite databases.

Verified scope includes fake/in-memory contracts, local filesystem behavior,
PostgreSQL transactions, SQL rollback, multi-client CAS, reopening ledgers and
failure injection. A memory adapter survives worker replacement only within the
same shared instance; it does not survive process loss. PostgreSQL ledger durability
does not imply that a process backend can rediscover an execution after registry loss.

Environment and validation limits:

- Linux cgroup v2/bubblewrap isolation is implemented as an optional backend.
  The modified Linux tests have not yet been executed in this revision.
  Production requires both cgroup v2 delegation and a trusted management process
  with tmpfs mount/unmount authority in the recovery mount namespace; see the
  [production conditions](docs/design/execution.md#linux本番管理processと受入条件).
  Payloads require a user namespace, uid/gid 65534, no capabilities and no_new_privs.
- macOS Seatbelt isolation depends on the host. It has only been reported successful
  in explicit runs outside an enclosing sandbox; the real-isolation test remains
  ignored in the normal suite. The profile permits listing the root directory.
  Descendant execution is unsupported: requests setting requires_descendants are
  rejected before acquisition; Seatbelt continues to deny fork.
- Actual database-kill, power-loss and failover tests have not been performed.
- Performance and sustained load have not been measured as benchmarks. The small
  concurrency-test timings in the design notes are not performance validation.
- Windows is unsupported. Other targets and real model/provider integrations are unverified.

The reference schema validator uses JSON Schema Draft 2020-12 with local
references; external references and other drafts are rejected. Trusted native tools must yield; hard CPU/RAM/stop guarantees
need a capable execution backend. Unsupported requirements are rejected.
Dropping a future is not proof that an external process stopped.
See [execution limits](docs/design/execution.md), [PostgreSQL scope](docs/design/postgres.md),
[public contracts](docs/design/api.md), [requirement catalog](docs/requirements/catalog.json)
and [requirement status](docs/design/requirements-trace.md).

## GitHub-hosted CI

[CI](.github/workflows/ci.yml) uses standard `ubuntu-24.04` VMs and
`macos-latest`, with read-only repository permissions, pinned action commits,
no private-service secrets and cancellation of older runs for the same ref.
Linux contract jobs use a disposable PostgreSQL 18 service and exercise both
Python feature settings, public examples, trace generation, pinned Rig source
hashes and package inventory. Separate isolation jobs require every expected
test name to finish `ok`: Linux default 6 / Python 7, macOS Seatbelt 1.

The Linux administrative supervisor runs through `sudo` to prepare/use the
cgroup subtree and bounded tmpfs. The payload requires a new user namespace,
uid/gid 65534 and no capabilities; a dedicated real-execution test checks this.
Environment refusal, skipped tests, missing results or incomplete recovery fail
the job. This workflow is prepared but has no confirmed hosted run yet.
A hosted success will establish that runner configuration only. Production
kernel/runtime images, delegation, mounts, recovery storage and actual provider
connections still require deployment-specific acceptance. See
[execution acceptance](docs/design/execution.md#github-hosted-ciの受入境界).

## Origin and licenses

Runtrell initially adopted selected code and tests from Rig v0.45.0, commit
`98ed06d7f1c911e6e4c7fdb5d690093bc964c514`, once. It is independently maintained,
does not track upstream, and is not an official Rig release. Rig API compatibility
is not promised.

New code is Apache-2.0 ([LICENSE](LICENSE)); Rig-derived code retains MIT
([LICENSES/MIT-Rig.txt](LICENSES/MIT-Rig.txt)). The mixed protocol crate declares
`MIT AND Apache-2.0`. Root and crate [NOTICE](NOTICE) files describe attribution;
each package includes its applicable license texts and notices.

[Adoption records](docs/adoption/adoption-manifest.md) retain original paths,
source hashes and derived paths. `xtask hashes` checks current derived-file origin,
copyright and license headers, plus the Rig MIT license text. It does not require
derived files to retain their initial extraction hashes. With `RIG_SNAPSHOT` set
to a local checkout of the pinned Rig snapshot, it also verifies adopted source
file hashes. Historical baseline logs/manifests are not part of this repository
or that check. See [provenance](docs/adoption/provenance.md).

## Name history

The project used the provisional name `agentloom` until 2026-10-25.
