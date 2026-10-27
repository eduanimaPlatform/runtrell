# PostgreSQL adapter（schema 4）

Storage failure events are emitted at the PostgreSQL adapter boundary. Each event records a static operation identifier, a five-character SQLSTATE when available, an allowlisted failure class, and a generated correlation ID for that diagnostic event. The public `Error::Storage` result carries no diagnostic payload; hosts must attach a safe request/run correlation field on the active tracing span when they need to group events across a request. The adapter does not log SQL text, bound values, database message or DETAIL, error Display/Debug output, connection strings, or input data.

The ERROR event target is `runtrell_postgres::storage`, with the fixed message `storage failure` and fields `operation`, `sqlstate` (empty when unavailable), `class`, and `correlation_id` (UUID). Classes are limited to `constraint`, `data_exception`, `transaction`, `connection`, `database`, `pool`, `configuration`, `decode`, `serialization`, and `conversion`.

The host owns the `PgPool` and tracing subscriber. It must disable sqlx statement logging on its connection options and filter out sqlx's own log targets from ordinary logs; those upstream events can contain SQL or PostgreSQL NOTICE text outside this adapter's control. Keep `runtrell_postgres::storage` enabled. No dynamic identifiers or request values may be passed as adapter operation names.

For example, a `tracing_subscriber::filter::Targets` filter can set `sqlx` to `LevelFilter::OFF` while enabling `runtrell_postgres::storage` at `LevelFilter::ERROR`. The `sqlx` target prefix covers query and `sqlx::postgres::notice` events. `storage_canary_host_filter_suppresses_sqlx_query_and_notice` exercises this at a TRACE default level with both canary SQL and a PostgreSQL warning.

RunStore / OperationStore / BudgetAuthority / Schedulerのtraitを維持し、空DBから最新schemaを作るmigrationに置き換える。リリース前のため旧schemaの検出・移行は持たない（空DBから作る）。DB schema version=4と、既存のRun/checkpoint wire schema=1は別の版である。

`PgStore::open`はhostのPgPool・schema名・Capacityを受け、初期化のCOMMIT後に返る。schema名はASCII英数字/underscore、先頭は英字/underscore、最大63 bytes、`pg_` prefix不可。識別子を検証・引用し、値はbindする。再open時のCapacity不一致はConflict。adapterはDocker操作や接続情報の自動読込みをしない。

## schemaと検索範囲

| table | 正本とindex |
|---|---|
| executions | 所有Run/operationの複合PK、固定request/Workspace入力JSONB、numeric revision、取消要求、最新観測、資源別条件/receipt JSONB、回収済み判定。未回収行の部分index |
| schema_config | 不変のversion/Capacity。通常の更新ではlockしない |
| runs | 受付入力request_spec（親期限適用前）と実効spec、Run単位のrevision・generation・owner/lease・checkpoint・終端/停止/回収/精算/配送状態・receipt。PK tenant/id、受付unique tenant/principal/request_key、親検索、session未完了sequenceのindex |
| sessions | 衝突しないtenant/session key、受付sequence、context_revision |
| attempts | 安定attempt ID、request/intent、送信世代/回数、result、external ID、usage観測、unsettled_result。Run/操作key/attempt unique、Runへの直接FK、Run未精算index。論理操作keyと有料attemptは区別する |
| budget_accounts | 親口座、上限・予約・実費の各Amount、unknown件数、exhausted、tokens/costそれぞれの警告配送済み閾値 |
| attempt_accounts | begin時のattempt/全祖先口座の帰属参照だけを保持。2列の複合PKと両方へのFK、口座検索index。予約・usage・unknown/settledは複写しない |
| budget_entries（view） | attempt_accountsとattemptsのJOINから予約・usage・unknown/settledを導出。現在残高はbudget_accountsの原子的な累積値を使う |
| slots | 保持key/kind/tenant/Run。key unique、Run/kind検索index |
| capacity_slots | scope/ordinalごとの有限permitとholder。global・tenant・Runのkind別、queueのglobal・tenant別。空きpermitの部分index、holder検索index |
| queue_entries | 安定key、挿入順sequence、tenant、優先度、deadline。tenant/優先度/順序とdeadlineのindex |
| queue_tenants / queue_policy | tenant候補、前回tenantと優先連続数。dispatch順序だけの小さな正本 |
| late_results | attempt/旧世代/worker/digestのappend-only観測、時刻、元result |

単一の`records(kind,key,data)`台帳は廃止する。revision、generation、sequence、時刻、状態などをentityごとの列に分け、spec・checkpoint・message・request・result・Amountなどの構造化値にはJSONBを使う。tenant、受付key、session、親、ownerの検索列は同じ正本からのgenerated列であり、二重書込みしない。

通常の更新は対象Runだけを読む。mark_sent/observe_result/proven_unsentは対象attempt、beginは対象論理操作のattempt群、recordは対象attemptと同Runの未精算attempt、finalize/listは対象Runのattempt群を読む。予算処理は対象口座と全祖先、session検査は当該sessionと先行未完了Runを最大1件読む。取消伝播は対象部分木だけを読む。queue全体の配列復元はしない。`queue_len`/`slot_count`の集計と`recovery_candidates`は明示的な管理用検索である。

## lock順序

1. 受付identityだけのadvisory lock（start/admit_child）。
2. 該当session行（sessionの新規業務/確定）。子受付は親/子のsessionをID昇順で先にlockする。
3. 対象Run行を`FOR UPDATE`。取消・期限/予算終端の伝播は親から子へlockし、各親のlock取得後に子を検索する。子受付も親行をlockするので、検索中に子孫が抜け落ちない。
4. 対象口座と全祖先のIDを不変の親参照から取得し、**口座ID昇順**の一つのSELECTで`FOR UPDATE`。階層の深さによらず全呼出しが同じ順序を使う。
5. 更新対象slot/queue、容量permit。slotの直接操作はslot keyだけのadvisory lockも使う。

checkpoint/renewは口座・容量のlockを取らない。全Run共通の更新lockは存在しない。schemaのadvisory lockはopenのDDL初期化時だけ使う。

容量割当はglobal→tenant→Runの各**空きpermit**を`FOR UPDATE SKIP LOCKED`で取得する。見つからなければ同じ空き条件を`FOR UPDATE`で再検索し、一時的にlockされた空きpermitを有限に待つ（既存のlock_timeout=5秒）。lock待ち後も空きがなければBackpressure、timeout・SQL失敗はStorageとする。更新処理の失敗は明示的な`rollback().await`の完了後に返し、途中までのpermit割当ても取り消す。占有permitや共通カウンタ行を待たない。global permitは初回利用時の初期化競合も避けるためopenで作る。tenant/Runのpermitは必要なscopeだけ作る。解放は該当holderのpermitだけをscope/ordinal順でlockする。lease失効だけではpermitを解放しない。

queueの期限切れentryと通常entryは`FOR UPDATE SKIP LOCKED`で選ぶ。既存契約のtenant round-robinと**tenantをまたぐ**優先連続数を厳密に保つため、通常dispatchには専用の`queue_policy`行を使う。これも`SKIP LOCKED`で取得し、他dispatcherが保持中なら待たずNoneを返す。host workerは一時的な候補なしとしてpollする。**queueの順序決定だけは短いtransactionで一つずつ行う**。Run更新・受付・予算・slot操作はこの行を取得せず、全Runのstore処理を直列化しない。entryを別transactionが保持中でも、空いている別entryを取得できる。

## transactionと保存barrier

| 境界 | 同じtransactionで確定するもの |
|---|---|
| 受付 | request同値照合、Run、session sequence、有限queueとpermit |
| 子受付 | 親claimのfence、固定入力spec同値照合、新規作成時の取消/終端/絶対期限/session/tenant/口座の検査、子Run/queue/permit、ChildRun slot、親操作のAccepted結果（child Run ID）。同じtransaction内でstartの遷移を再使用し、外部送信はしない |
| claim/renew | owner/generation/lease、Run revision、claim時のactive slotとqueue除去 |
| begin | 現owner/取消/期限/session、全祖先予約、attempt intent、帰属参照、kind別slot |
| mark_sent | 現ownerと新規業務のfence、送信世代/回数、再送保証の世代/scope/期限 |
| 結果 | 同値二重精算no-op/異値拒否、result/usage、全祖先の精算・unknown、slot解放、配送状態 |
| wait | Waitingの保存と親の実行用Run slot/permit解放。ChildRun/有料operationのslot・未精算予約は保持 |
| checkpoint | schema検査、Run revision CAS、履歴/cursor |
| finalize | 未確定操作・予算・receipt/hash/根拠/設定/検証器版、session context、completion/出力/Run終端 |
| 取消/回収 | 子孫伝播、queue除去、取消/停止/cleanupの独立状態。未精算予約は保持 |

親Policyの認可もtransaction外で行う。admit_childの新規受付と既存要求の再照会を分け、取消し・期限後の既存受付/精算はowner fenceで継続する。祖先Sessionと同じSessionを子が使用する受付は拒否する。祖先のSession/親関係は不変なので参照だけを取得し、逆順の祖先Run lockは追加しない。

外部model/toolのI/O中はDB transactionを保持しない。`synchronous_commit=on`でWAL flush後だけ成功を返し、保存要求の受領・memory更新と耐久保存完了を区別する。SQL/接続/COMMIT応答不明はStorageとし、安定identityから再照会する。自動再送はしない。更新のlock_timeout=5秒、statement_timeout=30秒。disk喪失・非同期failover・任意外部APIとのexactly-onceは保証しない。

古いownerのrecord/observe_resultは`late_results`へ耐久保存した後OwnershipLostを返し、残高・slot・成功状態に反映しない。現ownerが照合する。終端Runも残存slot・未完了cleanup/精算から再発見する。

`observe_result`で現ownerのfenceとRun/attempt帰属を検証して保存した完全なApplied/Failedは、driverのdispatch/reconcileがlookupより先に再利用する。十分性は保存されたrequestのprice・quantityとusageから判定し、欠測を0とはしない。不正なusage観測も精算根拠にはせず保持し、後から正しいlookup結果で補える。完全な保存結果は異値observe/recordで置換できず、proven_unsent/mark_sentによる再送も拒否する。現在のclaimと認可を再検証したrecordだけが全祖先を一回精算する。旧ownerのlate_resultsはこの経路に含めない。DBとControlStoreは信頼境界内であり、DB管理者の直接改ざんの検出は保証しない。

精算後はresultを正本とし、usage_observationsとunsettled_resultの重複を解放する。未精算の部分usage/結果は維持し、Unknown/Acceptedによる後続観測では上書きしない。late_resultsの保持期限・GCは決めない。attempt_accountsのFKにより、未精算・精算済みattemptと帰属先口座の削除制限を維持する。attemptsのRun FKと口座parentのFKも維持する。帰属参照はbeginの同じtransactionで全祖先分を一度だけ挿入し、送信・観測・精算では更新しない。完全導出だけでは失うattempt削除制限をこの小さい参照で守る。APIはRunの口座と口座parentを変更せず、GC APIは提供しない。口座別view照会には口座indexを使えるが、現在残高取得にはaccount/lock_account_inを使う。

## 検証と性能計測

Memory/Pg共通51件ずつと既存Pg固有17件を維持する。追加6件は独立Run行lock、独立口座/permit lock、48 Runの共有祖先予約、16 Workerのqueue取り出し、throughput、再open後の巡回/優先順序を検証する（旧schema拒否の試験は旧形式検出を持たない方針により削除）。

計測は専用DB `127.0.0.1:55433`、PostgreSQL 18.6、24 Runのrenewを各20回、合計480更新、concurrency=1/8/24で行う。値は当該環境の参考値でありSLO判定は行わない。巨大台帳の長期運用・DB kill/disk障害・非同期failoverは未検証。permit行数は設定容量と利用scope数に比例し、空いたtenant/Run用permit行のGCは今回実装しない。

### 2026-10-25の最終結果

指定DBでMemory/Pg共通102件（51件ずつ、89.03秒）・Pg固有24件が全件成功。workspace全体は609 passed / 0 failed / 1 ignored。ignoreは並行2d側の実隔離ホストを要する1件で、Pgにはignoreなし。build/clippy/fmt/hashes/package --list --allow-dirtyも成功。hashesは87採用ファイルのheaderとbaselineを検証し、RIG_SNAPSHOT未設定のため上流原本hashの再照合は0件。packageのmetadata未記載warningは残る。

48 Runを同時に予約させ、20件成功・28件ReservationDenied、共有上限400/400を維持した。独立Run行を別transactionが保持していても別tenantのrenewが1秒以内に成功し、独立口座/占有global permitの保持中にも別tenantのbeginが1秒以内に成功した。16 Workerによる64 queue entryの取り出しは64 unique・重複0・残件0。

throughput試験だけを選択した実行（24 Run、各20 renew、各条件480件）の結果：

| concurrency | 件数 | 所要時間 ms | 更新/秒 |
|---:|---:|---:|---:|
| 1 | 480 | 3567.939 | 134.53 |
| 8 | 480 | 1025.180 | 468.21 |
| 24 | 480 | 596.839 | 804.24 |

他テストと共走する全Pg試験実行の値とは区別する。プロセス全体やDBの他負荷まで隔離したbenchmarkではなく、SLOの達成判定や本番性能の保証はしない。試験結果は上表の範囲に限定する。

## 2e：耐久ExecutionLedger

PgStoreはExecutionLedgerを実装する。schema 4の同じ0001.sqlへexecutionsを追加し、旧schema検出・移行は追加しない。台帳はRunStore/OperationStoreと別の保存境界であり、外部取得前にintentのCOMMITを待つ。通常のSandbox受付・予約後のintent未保存は、既存operationから同じidentityで再照合する。台帳のCOMMITは業務成功や費用精算のreceiptではない。

ownerとcontext_operation、requestは不変。tenant/run_idはownerのgenerated列で検索し、全体records復元はしない。insertは同じPKの要求同値だけ冪等に返す。CASは対象行だけをFOR UPDATEし、Memoryと共通のExecutionRecord::advanceでrevision・取消・観測・資源条件/receiptを検証する。reclaimed列は同じtransactionでresourcesから算出し、pending()は未回収部分indexを使える管理検索とする。各資源の解放条件と確認はresources、証拠となる最新backend観測/費用はobservationに保存する。取消だけでは解放しない。

writeはsynchronous_commit=on、lock_timeout=5秒、statement_timeout=30秒。SQL失敗/COMMIT応答不明はStorageを返し、元identityで再照会する。既存のSQL失敗注入もExecutionLedgerのinsert/CASに適用する。ledgerは独立したhost契約なのでruns/attemptsへのFKを要求しない。hostが正しいRun/operationを接続する責任を持ち、ledger操作自体はRun claim認可の代わりではない。

Memory/Pgに同一6件の契約suiteを適用する（固定入力/identity、CAS/取消/条件、資源別receipt、client再生成後の回収/冪等性、registry全喪失、dispatch再生成後の同値照合/予約精算）。Pg追加試験ではinsert/CASのSQL失敗rollbackと2client同時CASの一勝一敗を確認する。backend再識別の境界は[execution](execution.md)を参照。PgStoreを作り直してもProcessBackendのmemory registryを復元することはできない。

## 予算・受付方針

AccountPolicyと追加数量の予約/使用/配送済み警告はbudget_accountsのJSONB列へ保存し、既存の全祖先lock順と同じtransactionで更新する。Run方針はspec、優先度/公平性の重みはqueue_entries.schedulingへ保存する。queue内の選択はMemoryと同じbudget::queue_indexを使う。schema 4は空namespace用であり、過去schemaの移行・互換読込みは追加しない。

## Host-owned transaction

`lock_account_in`、`start_in`、`admit_child_in`、`create_account_in`、`create_account_with_policy_in`、`terminate_in`、
`finalize_in` は `&mut sqlx::Transaction<'_, Postgres>` に参加する公開 API。
`start_in` は RunSpec.account で既存口座を指定でき、RunSpec.parent で親子関係を
指定する。独立した後続 Run も同じ口座を共有できる。受付の同 key・同内容は
同じ Run を返し、異内容は Conflict。親の期限適用前の受付内容（request_spec）を保存・比較し、元の child spec を再実行できる。
`admit_child_in(tx, claim, operation_key, spec, now)` は通常の子受付と同じ親fence・固定入力spec照合・slot・親操作保存を一つのsavepoint内で行う。固定入力は親のChildRun操作、実効受付specは子のrequest_specへ保存する。受付identityは親Run ID・操作key・元request_keyのJSON tupleで分離する。
通常の trait API と同じ遷移・lock・保存処理を使い、hook/callback は持たない。

host は同じ transaction で独自の SQL を実行し、最後に commit/rollback する。
各 API の成功は未 commit の staged write であり、耐久成功ではない。
API は savepoint を使い、失敗時はその呼出しの部分更新を戻す。host は一連の
処理のどれかが失敗したら外側も rollback する。外側の rollback では口座・Run・
queue/permit がすべて消え、予約や外部 I/O は開始しない。
`terminate_in` は成功以外の終端、`finalize_in` は検証済み成功の既存条件を守る。

同じ database/namespace の初期化済み store と通常の READ COMMITTED transaction
を使う。transaction 全体で既存の lock 順序を守り、SQL の待機は有限とする。
API は synchronous_commit=on、lock_timeout=5s、statement_timeout=30s を設定する。
host は commit 前にこれらを弱めず、外部 I/O 中は transaction を保持しない。

`lock_account_in(tx, tenant, id)` は同じ transaction で対象口座と全祖先を口座ID昇順に FOR UPDATE し、対象の Account（tenant・保存された明示的 policy・limit・現在残高）を返す。対象 tenant の不一致は PermissionDenied、不在は NotFound。policy や上限を既定値へ置換しない。lock は外側の commit/rollback まで保持する。

既存 Run を変更する transaction は上記の session → Run → 口座順を守る。既存 Run の finalize_in 等より先に口座を lock しない。口座を先に lock して後続 start_in を行う場合は、既存 Run の親・session を取得しない独立 Run の受付に限定する。同じ transaction 内の未 commit 新規口座・Run は使用できる。複数の独立した口座階層を逆順で追加 lock しない。

子受付の停止境界は`child_restart_across_processes`で検証する。試験adapterが通常のspawn_child/admit_child_inを呼び、子・親操作・queue/slotの未commit書込み後に外側transactionを保持して別OS processからSIGKILLする。DBの親行lock取得をrollback完了の同期証拠とし、部分的な残存がないことを検査する。commit後・応答前、子正常完了後・親join/採用/精算前でもkillし、独立process/poolで同じchild ID・保存completion・一回の親確定と親子の有料精算に収束する。sleepやtimeout延長を同期点には使わない。
