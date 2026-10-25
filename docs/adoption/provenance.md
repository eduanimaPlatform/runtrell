# 来歴と独立保守

初期採用元は https://github.com/0xPlaygrounds/rig のRig v0.45.0、commit
`98ed06d7f1c911e6e4c7fdb5d690093bc964c514`。
必要な実装とtestを初回のみ採用し、`runtrell-protocol`へ統合した。
[manifest](adoption-manifest.md)と[snapshot-files.json](snapshot-files.json)に元path、元hash、現在の派生pathを保持する。
部分移設したhelperは[snippet-sources.json](snippet-sources.json)で追跡する。
元ファイル全体の行数と採用部分の行数は区別する。

`snapshot_sha256`はコピー時点のbytes、`source_sha256`は採用元ファイル全体のSHA-256であり、一致する。
`extracted_sha256`と`extracted_lines`は初期抽出・帰属header追加・改変・fmt後の履歴値であり、現在内容のhashではない。
派生ファイルは独立に変更されるため、履歴値は書き換えない。

## 検査の対象

`cargo run -p xtask -- hashes`はrepo内だけで実行できる。現在の74派生ファイルについて、
先頭の出典（Rig版・commit・元path）、元著作権、MIT継承/Apache-2.0変更の表示を確認し、
`LICENSES/MIT-Rig.txt`の固定hashも照合する。派生内容全体と初期抽出hashの一致を要求する検査ではない。

`RIG_SNAPSHOT`に固定採用元のローカルcheckoutを指定した場合に限り、74採用元ファイルのhashも照合する。
未指定時は採用元hash検査を実施していない旨を出力する。部分移設helperの出典とhashは別台帳に保持する。
過去の基準build生ログ・baseline台帳は公開対象に含めず、xtaskも参照しない。
初期の比較結果は[initial-build](initial-build.md)、試験の対応は[test-map](test-map.md)に要約して残す。

## ライセンスと依存方向

Rig由来ファイルに`Copyright (c) 2024, Playgrounds Analytics Inc.`と改変・ライセンス表示を保持し、
MIT全文は`LICENSES/MIT-Rig.txt`に同梱する。Rigに採用対象NOTICEはなく、本repoのNOTICEで由来と適用範囲を補う。
新規部分・変更部分はApache-2.0であり、root LICENSEは元MITを置換しない。
crateごとのLICENSE・NOTICE・必要なMIT本文もpackageへ同梱する。
以前の社内実装の試験条件は仕様として再作成したが、そのコード・prompt・fixtureはコピーしていない。
変更と継承の対応は[reuse-map](reuse-map.md)を参照する。

protocolは内部判断に利用し、host APIへ内部型を再exportしない。
hostの非公開接続層から公開契約を利用する依存方向であり、逆向きの参照はない。
現在のruntime、Workspace/Artifact、PostgreSQL、ExecutionBackendの提供範囲と未検証項目は
[README](../../README.md)と[要件追跡](../design/requirements-trace.md)を参照する。
`Done`は判断上の終了候補であり、外部副作用・精算・Run成功の証拠にはならない。

初期採用後の上流merge/rebase同期/cherry-pick/backport/後続patch手動移植は行わない。
通常の独立依存やtoolchainの更新、不具合修正と再buildは独立して行う。
上流API・package名・保存形式の互換保証はなく、Rig公式版ではない。
