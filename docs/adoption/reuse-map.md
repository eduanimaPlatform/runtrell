# 再利用・変更・削除の対応

ファイル単位の対応とhashは[adoption-manifest](adoption-manifest.md)、test関数単位は[test-map](test-map.md)。
採用した意味をstubへ置換した箇所はない。

| 区分 | 元component | 抽出後 / 理由 | 対応検証 |
| --- | --- | --- | --- |
| 継承 | rig-agent/src/run/** | 同一crate run。判断・tool call/result順序・不正call・中断・serde・usageを維持 | run 104 unit / 13 doc |
| 変更 | run max_turns / RunSpec / PromptError::MaxTurns | field/API/判定を削除。巨大値への置換はしない。固定turn上限では止めず、共有予算と期限でI/O前に制御 | turn Red→Green、再開/継続、未知field拒否 |
| 変更 | run RUN_FORMAT / Done / Report | 独立format 1。Done/ReportをBox化し内容保持。Doneは候補 | 旧・不正format拒否、全run tests |
| 継承 | completion/message、native、citation、identity | multimodal/opaque/native/finish/call IDを維持。citation入力型だけwireから同namespaceへ移設 | message/native/citation、host opaque roundtrip |
| 変更 | operation/completion::canonical | Opaqueをreplay不可にする処理をrequest::continued内へ移設。operation crate全体は不要 | request/history tests |
| 継承 | conversation/** / pairing | call/resultの対応、repair、編集・著者・lineage・IDを維持 | conversation/pairing tests |
| 変更 | completion/history | provider WireIdsをホストのHashMap<CallId,String>へ置換。catalog factsを削除。ReplayTarget/Request/Memo/adapt_for/context/cache_pointsをhostへ公開 | history全tests、call ID rewrite |
| 継承・変更 | completion request/response/usage/cost/capabilities | required-field/finish/aborted/rawを保持。HTTP response::reportedは空ID除去だけ移設。pricingはホストから与えるデータのまま | request全汎用tests、metadata/unknown usage |
| 継承・変更 | options、mapping、merge、provider_options | portable options、JSON sections、strict refusalとmerge順位を継承。catalog規則・provider固有rewrite・wire Body変換・driver dry-runを削除。ホスト指定Set/Remove/MoveLastへ置換。ホストはprovider-specific optionsをJSON/typed extensionで渡せる | options/provider_options全generic tests、host JSON rewrite |
| 継承・選択 | streaming event / UnknownPayload | event順序・完全/partial transcript・unknown payload赤字化を継承。I/O stream reader、model driverとoperation foldは採用しない | event/unknown、run assembler、partial failure |
| 継承 | tool result/output/feedback/footprint/schema/validate | 実行結果の分類・拒否・副作用scope・schema検査・typed args schemaを維持。Executor/ToolContext/serveは外す | result/output/feedback/footprint/validate tests |
| 変更 | error.rs | VectorStoreError・HTTP/URL型・observe境界・provider envelope解釈を削除。分類/文字列/source chain/opaque JSON/retry verdictをhostから受ける。ProviderError::sourceは原本同様Arc内部を返す | error継承tests、source regression Red→Green、host failure metadata |
| 削除・不採用 | PromptError::Memory | conversation memory storeを採用しない。次段階のRunStoreとは別契約であり空storeを作らない | run 104 tests |
| 削除・不採用 | providers/embedding/vector/loaders/rerank/transcription/audio-image generation/client/catalog/driver/operation/wire/serve/telemetry | protocolに不要。transport/dispatcher/観測backendを製品へ持ち込まない | metadata/tree/FORK-04、独立build |
| 独自追加 | workspace / host_contract / host_driven | Public単独の組込みと、ファイル・Sandboxなしの構造化判断を実証 | integration 5、example実行 |
| 独立依存 | serde、schemars、tracing、sha2等 | 自社保守componentとは別供給元。更新可能。Tokio/HTTP transportは通常依存に不要 | dependency-licenses / metadata |

## 意図して維持する限界

structured_outputはJSON parseとtop-level required存在確認であり完全なJSON Schema validatorではない。
Tool-mode修復retryが尽きるとbest-effort候補になる場合がある。この候補を業務成功へ直結させない。
費用Costは上流の浮動小数点による情報表現を継承したもので、次段階の整数micro-USD予算台帳ではない。
HTTP/進捗streamのread/finish/error後poll停止はホストadapterの責務。
protocolは部分応答・中断理由とtool call/result対応を保持するが、外部処理停止・再送・費用精算を保証しない。

## 次段階に残す範囲

RunStore/OperationStore/BudgetAuthority/Finalizer、host認可・期限・取消と回収は未実装。
以前の社内実装の予算・回復・Runner・path越境の受入資産はその段階で適用する。
今回のcore抽出の成立と、全体設計の81件/28シナリオ達成を混同しない。

## runtrell-2a（段階1以後の追加）

前節の「未実装」は抽出完了時の記録。2aでは新規Apache-2.0の`runtrell`、`runtrell-testkit`、`xtask`を追加し、driver/公開契約/共有in-memory adapterを実装した。protocolのソースは変更せず内部判断に利用する。in-memory adapterは耐久保存ではない。

| 追加/意図した相違 | 理由 | 新しい受入 |
|---|---|---|
| DoneをFinalizerへ渡し検証/根拠/設定版・予算・取消を再照合 | protocolのbest-effortやJSON存在検査を業務成功に直結させない | U01/U06、exact budget、partial stream試験 |
| protocolのfloat Costを実行予算に使用しない | host価格snapshotと整数micro-USD、未計測≠0が必要 | Price/cap/unknown/retry試験 |
| malformed/不完全replyは理由付き非成功 | 2aは安全な最小経路、部分引数は実行しない | stream_partial_never_executes_tools_or_finalizes |
| 固定turn上限なし、物理的障害retryだけ有限 | 通常反復は予算と期限で管理 | 66 sample/260 tool、retry 3 attempt |
| model/tool/compaction/子/retryをdispatchへ統一 | intent保存と全階層予約の迂回を防ぐ | all_paid_paths_*、child_runs_* |
| 古いownerの遅延結果を現owner照会へ収束 | protocolの再発行だけで外部副作用を決めない | restart 10 case、cancel、deadline |
| crate直下にLICENSE/NOTICE/LICENSESを同梱 | rootの帰属文がpackageへ自動包含されない問題を修正 | cargo package --list（4 package） |

以前の社内実装の**実装コードは流用していない**。以前の社内実装の試験条件を仕様として再作成し（コードは非流用）、新APIに新規のテストを書いた。整数計算・mutex予約も新規実装であり、Pg実装・Codex由来コード/prompt/fixtureは持ち込まない。旧試験のCodex patch/compaction fixtureをコピーしない。

- store/tests：claim排他・stale fence、request/digest重複、実行済みattemptの保持、検証/取消競合、checkpoint原子性。
- store/budget_tests：共有口座、予約再使用、精算、50/80/95、unknown保持、invalid usage、同値二重精算、競合精算拒否。
- testkit/budget_execution：課金失敗→精算→retry、上限到達で確定禁止、単価不足、部分usage照会、再開receipt。
- testkit/model_notfound_recovery：resume/finalize/failed/unknown/in_progress/transport/expired/caller_changed/no_policy/handoffの10case。
- testkit/acceptance：共有口座のRun/worker切替、撤去された回数上限を超える継続、共通dispatch/取消/再開。Workspace/Sandbox/Artifact本体は次段階。
- agent/tests：同じ操作の照会、認可再検査、古いattemptの拒否、課金失敗・予算到達、stream不完全の非実行、圧縮失敗/同時履歴更新のCAS条件。圧縮本体/旧fallback文は不採用。

`xtask hashes`は現在の74派生ファイルの出典header・著作権・ライセンス表示とMIT本文を検査する。`RIG_SNAPSHOT`指定時だけ採用元snapshotのsource hashも照合する。抽出時hashは履歴記録であり、現在内容との一致を要求しない。baseline台帳・生ログは公開対象にも検査対象にも含めず、台帳は更新しない。

2aでは認可拒否のtool結果を継続モデルへ戻す以前の社内実装経路を採用せず、理由付き非成功で終了する。invokeのtransport/Errorは保守的に結果不明として扱い、既知の失敗・課金・retry条件はadapterが`ExternalResult::Failed`で明示する。実Providerの細かな分類・自動再生成は提供していない。

## runtrell-2c（PostgreSQL adapter）

| 追加/流用元 | 採用理由と境界 | 対応検証 |
|---|---|---|
| 2a MemoryStoreのApache-2.0状態遷移をruntrell-postgres内へ適用 | privateなMemoryStore台帳や非serdeのslot/queue公開型を変更せず、traitを固定したままDB transactionに移す。queue順・公平性巡回・全祖先予約・未知結果・世代fenceも耐久化。旧MemoryStoreの非耐久cloneを復旧根拠にしない | 同一runtime/fences/recovery/retry suiteをMemory/Pgに適用、SQL失敗rollback、複数接続/Runtime、pool全破棄後の復旧 |
| PostgreSQL control行のFOR UPDATEによる短期直列化（新規実装） | 全祖先口座・Run・操作・slot・queueを同一transactionで守る。以前の社内実装のaccount lock / SKIP LOCKED / generation fence実装・schema・migrationは読取りもコピーもしておらず、以前の社内実装由来の方式流用はなし。2aのgeneration契約に従う独自SQL adapter | Pg固有17試験と共通契約suite。初期構造の制約は後述のschema 2で置換 |
| 古い世代の応答をlate_resultsへappend-only保存 | OwnershipLostを返して精算更新を拒否しながら外部証跡を消さない。現ownerによる照会・照合と分離 | stale_response_is_durable_observation_without_settlement |

## runtrell-2b（新規Apache-2.0実装）

以前の社内実装の試験条件を仕様として再作成した（コードは非流用）。下表は試験条件の移植であり、旧コードの転載ではない。`data.rs` / `data/local.rs` / `file_tools.rs` / 圧縮promptは自社新規実装。Codex由来patch/compact/upstream/fixtureは採用0。

| 参照元 | 新配置・対応条件 | 相違・理由 |
|---|---|---|
| 以前の社内実装：Workspace境界/CAS試験 | `runtrell/tests/data.rs`：正規path、越境/link拒否、CAS、manifest、範囲/list/search、容量、原子的失敗、再open | 論理pathをdisk pathへ写さず、namespace状態fileを原子的に更新。旧patch試験の原子性条件をexact-replaceへ再記述。patch構文・fixturesは不採用 |
| 以前の社内実装：Workspace保存/削除試験 | 同 `local_reopen_*` / `retention_cleanup_*` / `same_run_child_agent_*` | 保存と削除を明示し、Run終端時の無条件discardは採用しない。世代/leaseは2a control-storeに残す |
| 以前の社内実装：Run/Workspace寿命試験 | 同 staging喪失/reopen/checkpoint Artifact、`testkit/tests/data_tools.rs`のVMなしRun | 実Sandbox/worker shutdown・Run終端からの自動削除連携はExecutionBackend段階で試験。未実施を継承済みとしない |
| 以前の社内実装：ファイルtool試験 | `testkit/tests/data_tools.rs`：共通dispatch、catalog/schema、write、検証、外部出力。`data.rs`：list/read/delete/search/editの条件 | 型・API・path/limitエラーを新設計へ書き直し。全6旧toolのコード移植ではない |
| 以前の社内実装：成果物検証/確定試験 | `Target` / `Evidence` / `DataValidator` / `DataFinalizer`、`artifact_basis_*` / `diagnostic_locations_*` / `same_validator_*` | 現行manifest・hash・basis/config/validator版と信頼済み保存receiptの照合を継承。業務Compiler/確定handlerはhost拡張 |

## runtrell-2c-fix：PostgreSQL schema 2

`crates/runtrell-postgres/src/engine.rs`の2a由来Apache-2.0状態遷移を維持し、単一control lockとrecords全量復元を削除した。`migrations/0001.sql`を空DB用schema 2へ置換し、新規`src/persistence.rs`にRun/attempt/祖先口座の限定検索、口座ID昇順lock、有限permit、SKIP LOCKED queueを実装した。以前の社内実装/Rig由来SQL・schemaは流用していない。新規依存と他crateの変更は不要。

検証対応は[PostgreSQL設計](../design/postgres.md)。共通51件×Memory/Pg、既存Pg固有17件、追加7件を対象とする。ExecutionLedgerの追加は後述の2eで行った。

## runtrell-2d（新規Apache-2.0実装）

以前の社内実装の隔離・回復試験条件を仕様として再作成した（コードは非流用）。実装・fixtureの転載はない。starlark-rustのAPIや旧runnerのunsafe pre_execを流用せず、専用helperでsafeなrustix setrlimitを適用してから隔離launcherをexecする。初期処理系の比較は[execution](../design/execution.md)。

| 旧試験の条件 | 新試験/理由 |
|---|---|
| 無限実行、statement/heap/出力の上限 | 2eで独自VM/fuel試験を仕様ごと廃止。processのloop/output試験、helper CPU停止、非対応RAM拒否を維持。Fakeは制限強制の証拠に数えない |
| wall timeout、実PID kill/reap、空scratch削除 | process supervisorはcaller Futureから分離しkill/waitする。失敗launcher・物理scratch削除・冪等destroyは実施。実隔離conformanceは明示ignore、実行試行失敗を報告 |
| 大量stdout/stderr、declared input以外の読取り拒否 | pipeを有限bufferで読む。入力snapshotの固定、出力fileをNOFOLLOW/regular/nlink=1/有限bytesで回収。link・hardlink・容量試験は実施 |
| Linux CPU/AS/FSIZE/NPROCのOS probe | CPU/NOFILE/FSIZEの設定読戻しとmacOS CPU busy-loop停止を実施。macOS appendでFSIZEを越えた実測を保存し、macOS file上限を拒否へ変更。Linux実probe、AS/NPROC/tree quotaは未実施・非対応を区別 |
| parent envを継承しない、cwd専用化 | env_clear、専用HOME/TMPDIR/PATH、stdin null、host設定の実行file allowlist。実隔離でのenv/fork/越境は未確認 |
| timeout後の次job成功 | 失敗環境を別Runへ再割当しない。管理Worker再生成後は元operationの状態照会・回収を繰り返す。未知費用は予約保持 |

旧runnerのテスト成功をそのままRuntrellの成功に数えない。実施範囲と制約は[execution](../design/execution.md)を参照。

2dのFD遮断は新しい単一thread helper内で`libc::close`する小さなunsafe境界。旧runnerのunsafe pre_execは採用しない。合成FDの非CLOEXEC継承を先に失敗させ、close後に成功する試験を追加した。

## runtrell-2e（意図した仕様変更）

2dのRegisterBackend / Program::Registers / Instruction / evaluate / fuelを撤去。小さな計算・構造変換は通常の信頼済みRust Toolと共通dispatchを使う。上流Rigの継承コード/fixturesの変更はない。新規依存なし。

2dのtestkit lifecycle 12件はFakeExecutionBackendへ置換し、取得/取消/遅延/未知費用/回収のassertionを維持。Fakeはprogramを評価しない試験専用実装で、外部inventory相当のregistryとclientを分離した。制限強制の実証には使わない。対応表は[test-map](test-map.md)。

PgStoreにExecutionLedgerを実装し、schema 2の0001へexecutionsを追加。Memory/Pg共通のExecutionRecord::advanceと6件ずつの契約suite、PgのSQL rollback/同時CAS試験を追加。新しいbackend clientからの外部inventory照会とregistry全喪失Unknownを分けて検証する。ProcessBackendの残存scratch隔離も実試験する。旧schema互換/移行は作らない。
