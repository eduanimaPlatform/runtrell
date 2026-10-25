# 初回採用manifest

元repo: https://github.com/0xPlaygrounds/rig / tag: v0.45.0 / SHA: `98ed06d7f1c911e6e4c7fdb5d690093bc964c514`。

固定原本は読取り専用。以下はMIT由来の採用ファイル。元SHAとコピー直後SHAは一致し、初期抽出時の変更後SHAも履歴として記録する。完全なhash・行数は[snapshot-files.json](snapshot-files.json)。

通常のRig内部crate依存は0。run→同一crateのcompletion/message/conversation/streaming/tool/error/json_utils。core相互参照も同一crateへ統合。test_utilsはcfg(test)だけ。継承testは[test-map](test-map.md)で対応付ける。

採用ファイル: **74件**。元ファイル合計 **30260行**、抽出・変更後 **28145行**（tests、doc、帰属headerを含む）。部分抽出したファイルの元行数は元ファイル全体なので、実採用行数とは異なる。

| 元path | 新path | 元行数→抽出後行数 | 内部依存 / tests |
| --- | --- | ---: | --- |
| `crates/rig-agent/src/run/batch.rs` | `crates/runtrell-protocol/src/run/batch.rs` | 94→97 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/calls.rs` | `crates/runtrell-protocol/src/run/calls.rs` | 178→181 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/committed.rs` | `crates/runtrell-protocol/src/run/committed.rs` | 93→96 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/committed/tests.rs` | `crates/runtrell-protocol/src/run/committed/tests.rs` | 177→179 | run/core契約; 継承test本体 |
| `crates/rig-agent/src/run/mod.rs` | `crates/runtrell-protocol/src/run/mod.rs` | 1812→1789 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/output.rs` | `crates/runtrell-protocol/src/run/output.rs` | 30→33 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/patch.rs` | `crates/runtrell-protocol/src/run/patch.rs` | 181→184 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/patch/tests.rs` | `crates/runtrell-protocol/src/run/patch/tests.rs` | 21→24 | run/core契約; 継承test本体 |
| `crates/rig-agent/src/run/policy.rs` | `crates/runtrell-protocol/src/run/policy.rs` | 366→369 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/policy/tests.rs` | `crates/runtrell-protocol/src/run/policy/tests.rs` | 212→215 | run/core契約; 継承test本体 |
| `crates/rig-agent/src/run/prepare.rs` | `crates/runtrell-protocol/src/run/prepare.rs` | 346→347 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/prepare/tests.rs` | `crates/runtrell-protocol/src/run/prepare/tests.rs` | 60→63 | run/core契約; 継承test本体 |
| `crates/rig-agent/src/run/response.rs` | `crates/runtrell-protocol/src/run/response.rs` | 382→300 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/spec.rs` | `crates/runtrell-protocol/src/run/spec.rs` | 135→128 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/spec/tests.rs` | `crates/runtrell-protocol/src/run/spec/tests.rs` | 72→74 | run/core契約; 継承test本体 |
| `crates/rig-agent/src/run/streamed.rs` | `crates/runtrell-protocol/src/run/streamed.rs` | 455→458 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-agent/src/run/streamed/tests.rs` | `crates/runtrell-protocol/src/run/streamed/tests.rs` | 634→637 | run/core契約; 継承test本体 |
| `crates/rig-agent/src/run/tests.rs` | `crates/runtrell-protocol/src/run/tests.rs` | 2282→2261 | run/core契約; 継承test本体 |
| `crates/rig-agent/src/run/transcript.rs` | `crates/runtrell-protocol/src/run/transcript.rs` | 49→52 | run/core契約; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/handle.rs` | `crates/runtrell-protocol/src/completion/handle.rs` | 86→89 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/history.rs` | `crates/runtrell-protocol/src/completion/history.rs` | 1796→1790 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/history/tests.rs` | `crates/runtrell-protocol/src/completion/history/tests.rs` | 1236→1230 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/message.rs` | `crates/runtrell-protocol/src/completion/message.rs` | 1514→1517 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/message/citation.rs` | `crates/runtrell-protocol/src/completion/message/citation.rs` | 368→372 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/message/citation/tests.rs` | `crates/runtrell-protocol/src/completion/message/citation/tests.rs` | 169→172 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/message/identity.rs` | `crates/runtrell-protocol/src/completion/message/identity.rs` | 262→265 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/message/native.rs` | `crates/runtrell-protocol/src/completion/message/native.rs` | 331→334 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/message/native/tests.rs` | `crates/runtrell-protocol/src/completion/message/native/tests.rs` | 209→212 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/message/tests.rs` | `crates/runtrell-protocol/src/completion/message/tests.rs` | 349→352 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/options.rs` | `crates/runtrell-protocol/src/completion/options.rs` | 408→383 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/options/mapping.rs` | `crates/runtrell-protocol/src/completion/options/mapping.rs` | 169→172 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/options/merge.rs` | `crates/runtrell-protocol/src/completion/options/merge.rs` | 959→627 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/options/tests.rs` | `crates/runtrell-protocol/src/completion/options/tests.rs` | 408→441 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/provider_options.rs` | `crates/runtrell-protocol/src/completion/provider_options.rs` | 463→493 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/provider_options/tests.rs` | `crates/runtrell-protocol/src/completion/provider_options/tests.rs` | 741→742 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/request.rs` | `crates/runtrell-protocol/src/completion/request.rs` | 1874→1891 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/completion/request/plain_value_tests.rs` | `crates/runtrell-protocol/src/completion/request/plain_value_tests.rs` | 266→269 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/request/response_identity_tests.rs` | `crates/runtrell-protocol/src/completion/request/response_identity_tests.rs` | 18→21 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/completion/request/tests.rs` | `crates/runtrell-protocol/src/completion/request/tests.rs` | 884→788 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/conversation/aside.rs` | `crates/runtrell-protocol/src/conversation/aside.rs` | 101→104 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/conversation/author.rs` | `crates/runtrell-protocol/src/conversation/author.rs` | 122→125 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/conversation/lineage.rs` | `crates/runtrell-protocol/src/conversation/lineage.rs` | 41→44 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/conversation/mod.rs` | `crates/runtrell-protocol/src/conversation/mod.rs` | 1362→1365 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/conversation/note.rs` | `crates/runtrell-protocol/src/conversation/note.rs` | 57→60 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/conversation/pairing.rs` | `crates/runtrell-protocol/src/conversation/pairing.rs` | 712→715 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/conversation/pairing/tests.rs` | `crates/runtrell-protocol/src/conversation/pairing/tests.rs` | 423→426 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/conversation/tests.rs` | `crates/runtrell-protocol/src/conversation/tests.rs` | 599→602 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/conversation/turn.rs` | `crates/runtrell-protocol/src/conversation/turn.rs` | 103→106 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/error.rs` | `crates/runtrell-protocol/src/error.rs` | 907→409 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/error/tests.rs` | `crates/runtrell-protocol/src/error/tests.rs` | 665→201 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/id.rs` | `crates/runtrell-protocol/src/id.rs` | 130→133 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/id/tests.rs` | `crates/runtrell-protocol/src/id/tests.rs` | 21→24 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/json_utils.rs` | `crates/runtrell-protocol/src/json_utils.rs` | 448→451 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/json_utils/tests.rs` | `crates/runtrell-protocol/src/json_utils/tests.rs` | 212→215 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/streaming/event.rs` | `crates/runtrell-protocol/src/streaming/event.rs` | 339→342 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/streaming/event/tests.rs` | `crates/runtrell-protocol/src/streaming/event/tests.rs` | 180→183 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/streaming/mod.rs` | `crates/runtrell-protocol/src/streaming/mod.rs` | 308→42 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/streaming/unknown_payload_tests.rs` | `crates/runtrell-protocol/src/streaming/unknown_payload_tests.rs` | 26→29 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/structured_output.rs` | `crates/runtrell-protocol/src/structured_output.rs` | 107→110 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/test_utils/completion.rs` | `crates/runtrell-protocol/src/test_utils/mod.rs` | 583→69 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/test_utils/trace_capture.rs` | `crates/runtrell-protocol/src/test_utils/trace_capture.rs` | 283→287 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/tool/feedback.rs` | `crates/runtrell-protocol/src/tool/feedback.rs` | 213→216 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/tool/feedback/tests.rs` | `crates/runtrell-protocol/src/tool/feedback/tests.rs` | 139→142 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/tool/footprint.rs` | `crates/runtrell-protocol/src/tool/footprint.rs` | 169→172 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/tool/footprint/tests.rs` | `crates/runtrell-protocol/src/tool/footprint/tests.rs` | 136→116 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/tool/output.rs` | `crates/runtrell-protocol/src/tool/output.rs` | 243→246 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/tool/output/debug_tests.rs` | `crates/runtrell-protocol/src/tool/output/debug_tests.rs` | 28→31 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/tool/output/tests.rs` | `crates/runtrell-protocol/src/tool/output/tests.rs` | 68→71 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/tool/result.rs` | `crates/runtrell-protocol/src/tool/result.rs` | 566→566 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/tool/result/tests.rs` | `crates/runtrell-protocol/src/tool/result/tests.rs` | 104→107 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/tool/schema.rs` | `crates/runtrell-protocol/src/tool/schema.rs` | 89→92 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/tool/validate.rs` | `crates/runtrell-protocol/src/tool/validate.rs` | 460→464 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |
| `crates/rig-core/src/tool/validate/tests.rs` | `crates/runtrell-protocol/src/tool/validate/tests.rs` | 159→162 | 同一crate内の汎用型; 継承test本体 |
| `crates/rig-core/src/wire/citation.rs` | `crates/runtrell-protocol/src/completion/message/citation/input.rs` | 68→71 | 同一crate内の汎用型; sibling tests / doctests（test-map参照） |

## 部分継承・移設

`operation/completion.rs::canonical`（1195–1203行）のOpaque replay無効化は`completion/request.rs::continued`へ移設。operation型・driverは採用しない。
`provider_response.rs::reported`（381–384行）の空ID除去は`completion/request.rs`内へ移設。HTTP response型は採用しない。
`tool/schema.rs`はfeedbackの型検証testsに必要なargs_schemaを保持するため追加採用。

新規ファイルは`lib.rs`、`completion/mod.rs`、`tool/mod.rs`、`tests/host_contract.rs`、`examples/host_driven.rs`、workspace/manifest、採用資料とRust検査タスク。`test_utils/mod.rs`は上流のrefuse_options/each_set部分に新規Targetを追加した混在ファイル。

Apache-2.0新規部分とMIT継承部分を含むためcrate licenseは`MIT AND Apache-2.0`。独立依存のlicenseとは別の対象であり、これを推移依存全体のlicenseとして扱わない。
