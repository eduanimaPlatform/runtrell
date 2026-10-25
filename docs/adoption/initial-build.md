# 初期build比較記録

## 比較基準（既存調査の再利用）

原本: Rig v0.45.0 / 98ed06d7f1c911e6e4c7fdb5d690093bc964c514。
既存調査の基準結果を以下に要約する。原本のbuild/testは本公開準備では再実行していない。
生ログ・baseline台帳は公開対象に含めず、xtaskの検査対象にも含めない。
当時のrun-only構成はproviders等を含むcore/httpを依存に残していた。今回の独立抽出と同一構成ではない。

| 基準コマンド | 結果 |
| --- | --- |
| cargo build --locked -p rig-agent --no-default-features | 成功 |
| cargo build --locked -p rig-agent | 成功 |
| cargo test --offline -p rig-agent --lib（調査最終run-only構成） | run unit 104成功 |
| cargo test --offline -p rig-agent --doc（同構成） | run doc 13成功（通常9 + compile_fail4） |
| cargo test --locked -p rig-core --lib --tests -- --test-threads=4 | unit 1507 + integration 33 = 1540成功 |
| cargo test --locked -p rig-agent --lib --tests -- --test-threads=4（原本構成） | unit 391 + integration 17 = 408成功、2 ignored（API key必須） |

基準環境: RUSTUP_TOOLCHAIN=1.98.0、aarch64-apple-darwin、LLVM 22.1.8、CARGO_BUILD_JOBS=4、CARGO_INCREMENTAL=1、CARGO_TARGET_DIR=$TMPDIR/rig-target。
原本のrust-toolchain.toml/MSRV宣言は1.97.1。未導入のため調査時も1.98.0を利用した。
原本Cargo.lock SHA-256: `733c427ab5decd124dacaff1d5d1855a83ce05ee4827e9b64935edea8ea3d6bd`。
調査のcore 1507件すべてを採用した、という意味ではない。

## 抽出後の環境・lock

新rootのrust-toolchain.tomlは`channel = "1.98.0"`を明示し、workspace rust-versionは1.98とした。
1.97.1はinstallせず、成立も主張しない。元toolchainファイルは変更していない。
rustc 1.98.0 (88d9e12ae 2026-08-18)、cargo 1.98.0 (797e8a9bc 2026-08-05)、LLVM 22.1.8、aarch64-apple-darwin。
CARGO_TARGET_DIRは全Cargo実行で`$PWD/target`。
新規ツールinstallなし。依存は既存Cargo cacheからofflineで解決できた。
原本lockをコピーし、workspace変更に合わせ初回cargo checkでlockを再解決・pruneした後に--locked buildを実行した。
新lock SHA-256: `95e6ee824111d0c9092edb30b7080c8eb28ad24b2e703a6f9f49acbc9c0a45b6`。
原本lockにない外部版はlazy_static 1.5.1、smallvec 1.16.4、thread_local 1.1.10（test-only tracing-subscriber閉包）。

## 最終実行結果

以下は初期抽出時の記録であり、現在のworkspace検証結果ではない。

| コマンド | exit | 結果 |
| --- | ---: | --- |
| cargo build --workspace --locked | 0 | 成功 |
| cargo test --workspace | 0 | unit 334 / integration 5 / doc 53（通常49 + compile_fail4）すべて成功、失敗0、ignored0。 |
| cargo clippy --workspace --all-targets -- -D warnings | 0 | 警告0。 |
| cargo fmt --check | 0 | 成功 |
| cargo tree | 0 | rig-* 0。 |
| cargo run --example host_driven | 0 | fake lookup→構造化判断。ファイル/Sandbox/networkなし。 |
| RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --offline | 0 | 公開docsの壊れた参照を修正して成功。 |

unit内訳はrun 104、採用core関連230（うちJSON rewrite独自追加1）。integration 5は独自host契約。
run doctestは13件（通常9・compile_fail4）すべて継承。core関連doc40件。
core単体の原本1507件との件数一致を要求せず、採用した契約だけを移行して[test-map](test-map.md)で追跡する。

## 失敗からの修正と未検証

初回の依存切断後compile不足、古いprovider doc参照、帰属追加後の整形等は修正後に再検証した。
意図したturn上限撤去は325成功/2期待失敗から成功へ、未知max_turns拒否は3成功/1期待失敗から成功へ進めて確認した。
古いMaxTurnsを参照する履歴整合testも現契約に更新し、canonical historyのassertionは維持した。
error source downcastの退行は原本のwrapped_document_error testが検出し、Arc内部をsourceとして返す原本契約へ修正した。
Clippyのenum/errorサイズ差はDone/ReportのBox化、型複雑度はlocal alias、testの新規警告2件は不要doc空行削除/next_backへ修正した。lint抑制で通していない。

初期抽出時の未検証: Linux/WASM/1.97.1、real provider、DB・永続保存・共有予算・外部操作回復・実Sandbox・Finalizer・性能。
それらは段階1の採用/抽出を越える。外部I/Oや元driverのpoll/finish契約はホストadapterへ接続する責務として記録し、成功扱いにしない。
固定原本はHEAD一致・git status --porcelain空を最終再確認した。
