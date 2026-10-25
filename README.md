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

## Requirement acceptance

<!-- requirement-summary -->
分類は責任の配置、状態は要件全体の到達点であり独立した軸。公開ソースに存在することや試験件数は、134要件の全達成を意味しない。利用側の責任のfake接続試験は、実hostでの業務受入の証明ではない。

| 分類 | 検証済み | 設計済み | 未着手 | 合計 |
|---|---:|---:|---:|---:|
| 共通基盤 | 43 | 66 | 0 | 109 |
| 公開の任意機能 | 1 | 12 | 0 | 13 |
| 利用側の責任 | 0 | 10 | 2 | 12 |
| 全体 | 44 | 88 | 2 | 134 |

公開範囲: 共通実行基盤と選択可能な公開tool/backend/adapter。業務接続・設定と業務ロジックはhost側の別責務。方針型と共通実装はRuntrell、具体値はhostが供給する。ファイル操作・検索・画像生成/編集・Python・標準モデル接続等の再利用可能な実装も任意機能として扱い、全Runへ強制しない。この分類はそれら全ての実装完了を主張しない。

試験件数: CIの各feature・suiteのlibtest結果を実行時の正本とする（同じ試験のfeature別実行を独立要件数へ加算しない）。Linux実隔離はdefault 6件／python 7件、macOS実隔離は1件を名前とok結果で照合する。Hostedの成功はworkflow runの固定commit・runner・結果を確認して初めて記録する。現時点ではHosted実行未確認。本番構成・実provider接続・電源断・failover・負荷性能は未検証。
<!-- /requirement-summary -->

## Local validation

The pinned toolchain is Rust 1.98.0.

```sh
cargo build --workspace --locked
# Use a dedicated disposable local PostgreSQL container/database.
# The password below is only an example for that local test container.
export RUNTRELL_TEST_DATABASE_URL='postgres://postgres:runtrell@127.0.0.1:55433/postgres'
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
cargo run -p runtrell-testkit --example support_decision
cargo run -p runtrell-testkit --example alternate_host
cargo run -p xtask -- trace
cargo run -p xtask -- hashes
cargo package --workspace --list --allow-dirty
```

PostgreSQL tests require `RUNTRELL_TEST_DATABASE_URL`; they fail rather than skip
when it is absent. Tests create unique schemas and write test records. Point this
variable only at a disposable test database. To run just the adapter tests, use
`cargo test -p runtrell-postgres` with the same variable.

Verified scope includes fake/in-memory contracts, local filesystem behavior,
PostgreSQL transactions, SQL rollback, multi-client CAS, reopening ledgers and
failure injection. A memory adapter survives worker replacement only within the
same shared instance; it does not survive process loss. PostgreSQL ledger durability
does not imply that a process backend can rediscover an execution after registry loss.

Environment and validation limits:

- Linux cgroup v2/bubblewrap isolation is implemented as an optional backend.
  Real Linux execution and hosted CI results have not yet been verified.
- macOS Seatbelt isolation depends on the host. It has only been reported successful
  in explicit runs outside an enclosing sandbox; the real-isolation test remains
  ignored in the normal suite. The profile permits listing the root directory.
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
