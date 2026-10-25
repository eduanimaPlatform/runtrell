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

- Linux bubblewrap isolation has **not** been exercised on a real Linux host.
- macOS Seatbelt isolation depends on the host. It has only been reported successful
  in explicit runs outside an enclosing sandbox; the real-isolation test remains
  ignored in the normal suite. The profile permits listing the root directory.
- Actual database-kill, power-loss and failover tests have not been performed.
- Performance and sustained load have not been measured as benchmarks. The small
  concurrency-test timings in the design notes are not performance validation.
- Windows is unsupported. Other targets and real model/provider integrations are unverified.

The reference schema validator implements a small explicit subset and rejects
unsupported keywords. Trusted native tools must yield; hard CPU/RAM/stop guarantees
need a capable execution backend. Unsupported requirements are rejected.
Dropping a future is not proof that an external process stopped.
See [execution limits](docs/design/execution.md), [PostgreSQL scope](docs/design/postgres.md),
[public contracts](docs/design/api.md), [requirement catalog](docs/requirements/catalog.json)
and [requirement status](docs/design/requirements-trace.md).

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
