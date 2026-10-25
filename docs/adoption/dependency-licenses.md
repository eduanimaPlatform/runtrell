# 依存とライセンス

## 採用したRig由来コード

MIT。Copyright (c) 2024, Playgrounds Analytics Inc. 原文は`LICENSES/MIT-Rig.txt`。元root LICENSEと同一SHA-256: `42cf0f78132fe1de338cbff947bf5b9f7d2d8a377cb827c9df9e90b4b9499ef7`。新規・改変部分Apache-2.0と区別し、crate全体は`MIT AND Apache-2.0`。

## 初回抽出時の推移依存license宣言集計（現在のworkspace総数ではない）

cargo metadata --locked --format-version 1のresolve graphを起点crateから巡回。runtimeはnormal/build、test集合はdevも含む。name/version別。全target候補とnativeのmetadataを分ける。manifestの宣言集計であり、全配布物の法律監査や他targetのbuild成立を示さない。

| 集合 | 外部package数 | license不明 | Rig内部crate |
| --- | ---: | ---: | ---: |
| all-targets/runtime | 79 | 0 | 0 |
| all-targets/tests | 112 | 0 | 0 |
| native/runtime | 42 | 0 | 0 |
| native/tests | 72 | 0 | 0 |

| name | version | license宣言 | 集合 |
| --- | --- | --- | --- |
| anyhow | 1.0.104 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| autocfg | 1.5.0 | Apache-2.0 OR MIT | all-targets/tests, native/tests |
| base64 | 0.22.1 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| bit-set | 0.8.0 | Apache-2.0 OR MIT | all-targets/tests, native/tests |
| bit-vec | 0.8.0 | Apache-2.0 OR MIT | all-targets/tests, native/tests |
| bitflags | 2.13.1 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/tests |
| block-buffer | 0.10.4 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| bumpalo | 3.20.2 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| cpufeatures | 0.2.17 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| crypto-common | 0.1.7 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| digest | 0.10.7 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| dyn-clone | 1.0.20 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| equivalent | 1.0.2 | Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| errno | 0.3.14 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| fastrand | 2.4.1 | Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| fnv | 1.0.7 | Apache-2.0 / MIT | all-targets/tests, native/tests |
| foldhash | 0.1.5 | Zlib | all-targets/runtime, all-targets/tests |
| futures-core | 0.3.34 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| futures-task | 0.3.34 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| futures-util | 0.3.34 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| generic-array | 0.14.7 | MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| getrandom | 0.3.4 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| getrandom | 0.4.2 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| hashbrown | 0.15.5 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| hashbrown | 0.17.0 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| heck | 0.5.0 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| id-arena | 2.3.0 | MIT/Apache-2.0 | all-targets/runtime, all-targets/tests |
| indexmap | 2.14.0 | Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| itoa | 1.0.18 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| js-sys | 0.3.106 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| lazy_static | 1.5.1 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| leb128fmt | 0.1.0 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| libc | 0.2.186 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| linux-raw-sys | 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/tests |
| log | 0.4.33 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/tests |
| memchr | 2.8.0 | Unlicense OR MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| nu-ansi-term | 0.50.3 | MIT | all-targets/tests, native/tests |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| once_cell | 1.21.4 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| pin-project-lite | 0.2.17 | Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| ppv-lite86 | 0.2.21 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| prettyplease | 0.2.37 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| proc-macro2 | 1.0.106 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| proptest | 1.11.0 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| quick-error | 1.2.3 | MIT/Apache-2.0 | all-targets/tests, native/tests |
| quote | 1.0.45 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| r-efi | 5.3.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | all-targets/tests |
| r-efi | 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | all-targets/runtime, all-targets/tests |
| rand | 0.9.4 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| rand_chacha | 0.9.0 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| rand_core | 0.9.5 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| rand_xorshift | 0.4.0 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| ref-cast | 1.0.25 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| ref-cast-impl | 1.0.25 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| regex-syntax | 0.8.10 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| rustix | 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/tests, native/tests |
| rustversion | 1.0.22 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| rusty-fork | 0.3.1 | MIT/Apache-2.0 | all-targets/tests, native/tests |
| schemars | 1.2.1 | MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| schemars_derive | 1.2.1 | MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| semver | 1.0.28 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| serde | 1.0.228 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| serde_core | 1.0.228 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| serde_derive | 1.0.228 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| serde_derive_internals | 0.29.1 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| serde_json | 1.0.150 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| sha2 | 0.10.9 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| sharded-slab | 0.1.7 | MIT | all-targets/tests, native/tests |
| slab | 0.4.12 | MIT | all-targets/runtime, all-targets/tests |
| smallvec | 1.16.4 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| syn | 2.0.117 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| syn | 3.0.3 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| tempfile | 3.27.0 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| thiserror | 2.0.18 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| thiserror-impl | 2.0.18 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| thread_local | 1.1.10 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| tracing | 0.1.44 | MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| tracing-attributes | 0.1.31 | MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| tracing-core | 0.1.36 | MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| tracing-log | 0.2.0 | MIT | all-targets/tests, native/tests |
| tracing-subscriber | 0.3.23 | MIT | all-targets/tests, native/tests |
| typenum | 1.20.0 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| unarray | 0.1.4 | MIT OR Apache-2.0 | all-targets/tests, native/tests |
| unicode-ident | 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| unicode-xid | 0.2.6 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| uuid | 1.26.1 | Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| valuable | 0.1.1 | MIT | all-targets/runtime, all-targets/tests |
| version_check | 0.9.5 | MIT/Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| wait-timeout | 0.2.1 | MIT/Apache-2.0 | all-targets/tests, native/tests |
| wasip2 | 1.0.3+wasi-0.2.9 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wasip3 | 0.4.0+wasi-0.3.0-rc-2026-01-06 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wasm-bindgen | 0.2.129 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| wasm-bindgen-macro | 0.2.129 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| wasm-bindgen-macro-support | 0.2.129 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| wasm-bindgen-shared | 0.2.129 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests |
| wasm-encoder | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wasm-metadata | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wasmparser | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| web-time | 1.1.0 | MIT OR Apache-2.0 | all-targets/runtime, all-targets/tests, native/runtime, native/tests |
| windows-link | 0.2.1 | MIT OR Apache-2.0 | all-targets/tests |
| windows-sys | 0.61.2 | MIT OR Apache-2.0 | all-targets/tests |
| wit-bindgen | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wit-bindgen | 0.57.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wit-bindgen-core | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wit-bindgen-rust | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wit-bindgen-rust-macro | 0.51.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wit-component | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| wit-parser | 0.244.0 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | all-targets/runtime, all-targets/tests |
| zerocopy | 0.8.50 | BSD-2-Clause OR Apache-2.0 OR MIT | all-targets/tests, native/tests |
| zerocopy-derive | 0.8.50 | BSD-2-Clause OR Apache-2.0 OR MIT | all-targets/tests |
| zmij | 1.0.21 | MIT | all-targets/runtime, all-targets/tests, native/runtime, native/tests |

Tokio/HTTP/native transportは通常依存にない。tracingは原本の診断イベント用でありtelemetry backendは含めない。tracing-subscriberとproptestは継承testsだけ。FORK-04: metadataとcargo treeでrig-*は0、manifestの[patch]/git依存は0、.gitmodules/submoduleなし。Cargo.lockで独立依存を固定する。

直接依存の理由: serde/serde_json=message/state/optionsの往復とJSON補助、schemars=構造化出力/schema、thiserror=分類付き失敗、base64=履歴のmultimodal扱い、sha2=canonical fingerprint、uuid/fastrand=call/message ID、web-time=会話時刻。完全な源流条件・選択肢は上表のAND/ORを維持する。

## 2bの直接依存追加

`runtrell`が既存lockfileの `rustix 1.1.4` を通常依存（fs feature）として利用し、NOFOLLOW/openat/flock/rename/fsyncをsafe Rustから呼ぶ。宣言は `Apache-2.0 OR Apache-2.0 WITH LLVM-exception OR MIT`。`tempfile 3.27.0` はdev依存のみ（MIT OR Apache-2.0）。新しいクラウドSDKや外部Agent実装を追加しない。上の抽出時集計は当時の範囲であり、並行追加中のruntrell-postgresを含む現workspace全体の再監査値ではない。

## 2dの直接依存追加

2dではprocess supervisorのため`tokio 1.54.1`（MIT）を`runtrell`の通常依存へ追加する。既存lockfile内の版を使用し、HTTP/クラウドtransportは追加しない。新規lock entryは`signal-hook-registry 1.4.8`（MIT OR Apache-2.0、crate manifest確認）のみ。`rustix`へprocess featureを追加し、専用helperのsetrlimit/execに利用する。`libc 0.2.186`（MIT OR Apache-2.0）は既存の推移依存から直接依存へ追加し、新しいhelper process内だけで継承raw FDをcloseするために利用する。ライブラリ本体はunsafe禁止を維持する。上の旧「Tokioは通常依存にない」は初回抽出時の記録。


## rt-3c Schema評価器

`jsonschema = { version = "0.49", default-features = false }`、lockは0.49.9、MIT、MSRV 1.85。採用元の[0.49.9ドキュメント](https://docs.rs/jsonschema/0.49.9/jsonschema/)とキャッシュ済みCargo.toml/README/LICENSEを確認。公開の[保守元](https://github.com/Stranger6667/jsonschema)では後続版も公開されており、採用系列と最新版は区別する。HTTP/file resolver・TLSの既定featureを無効化し、追加依存はキャッシュからオフライン解決した。独自評価器の保守をやめ、共通の標準評価器へ委譲する。未知keyword/外部参照/別dialectを前処理で拒否する方針はRuntrell側が保守する。

今回lockへ追加した依存のmanifest license宣言（全配布物の法律監査ではない）：

| package | version | license宣言 |
|---|---|---|
| ahash | 0.8.12 | MIT OR Apache-2.0 |
| aho-corasick | 1.1.5 | Unlicense OR MIT |
| borrow-or-share | 0.2.4 | MIT-0 |
| bytecount | 0.6.9 | Apache-2.0/MIT |
| data-encoding | 2.11.1 | MIT |
| email_address | 0.2.9 | MIT |
| fancy-regex | 0.19.2 | MIT |
| fluent-uri | 0.4.1 | MIT |
| foldhash | 0.2.0 | Zlib |
| fraction | 0.15.4 | MIT OR Apache-2.0 |
| jsonschema | 0.49.9 | MIT |
| jsonschema-regex | 0.49.9 | MIT |
| jsonschema-value | 0.49.9 | MIT |
| micromap | 0.3.0 | MIT |
| num | 0.4.3 | MIT OR Apache-2.0 |
| num-bigint | 0.4.8 | MIT OR Apache-2.0 |
| num-cmp | 0.1.0 | MIT/Apache-2.0 |
| num-complex | 0.4.6 | MIT OR Apache-2.0 |
| num-rational | 0.4.2 | MIT OR Apache-2.0 |
| outref | 0.5.2 | MIT |
| referencing | 0.49.9 | MIT |
| regex | 1.12.3 | MIT OR Apache-2.0 |
| regex-automata | 0.4.18 | MIT OR Apache-2.0 |
| strum | 0.28.0 | MIT |
| strum_macros | 0.28.0 | MIT |
| unicode-general-category | 1.1.0 | Apache-2.0 |
| uuid-simd | 0.8.0 | MIT |
| vsimd | 0.8.0 | MIT |
