# 状態と保存項目

全ての更新はtenant・ID・owner generation・lease有効期限を検査する。終端Runでも既存operation照合、精算、cleanupを許す。in-memory adapterは同一共有instance内だけの整合性試験であり、process停止耐性を主張しない。

| 対象 | 遷移 | 保存項目・条件 |
|---|---|---|
| Run | Queued → Running → Waiting / Finalizing → Succeeded / Failed / Cancelled / DeadlineReached / BudgetReached / OutcomeUnknown | 固定RunSpec、schema、revision、checkpoint、output、reason、completion ID。waitingはslot/job/retryのみ。成功はFinalizer receipt保存後 |
| Session | 入力受付 → sequence採番 → context CAS更新 | tenant/session ID、入力順、context revision。別Runとして受付、同じexpected版への二重確定を拒否 |
| Ownership | 未所有 → leased(generation+1) → lease期限 / 新世代 | worker、generation、lease_until。取得後正本再読込。Lookup後の再送直前にも再検査 |
| Operation | Intent → Sent → Applied / Accepted / Failed / Unknown | 安定ID、要求digest、request本体、descriptor版/account、予約、attempt、external ID、owner世代、result。Acceptedは反映済みではない |
| Attempt | prepared → sent → settled / unknown / proven-not-sent | attempt番号、送信時刻、scope/retention、元request、予約ID。retryは元Run期限/口座を継承。結果不明は無条件retryしない |
| Reservation | reserved → unknown / settled | 全account祖先、予約tokens/cost、実績区分、価格snapshot。unknownは保持し新しい課金/確定を止める |
| Usage | unmeasured → measured | input/output/cached/reasoning、model、整数費用。cached/reasoningは内数、二重加算しない。重複精算は同値のみ許可 |
| Checkpoint | schema検査 → revision CAS commit | 内部判断状態、公開履歴、operation index、目的/制約/根拠/未完了操作。古い圧縮結果は保存しない |
| Cancellation | none → requested | 要求者/理由/時刻。新規業務I/Oを禁止、既存照合は継続 |
| Stop | not_required / pending → confirmed / unknown | 外部job・実体世代・停止証拠。future dropでconfirmedにしない |
| Cleanup | not_required / pending → reclaimed / unknown | 資源別ID・予約・試行・確認時刻。期限到達でreclaimedにしない |
| Settlement | pending → settled / unknown | 未確定operation集合。停止/cleanupと独立 |
| Delivery | none → accepted → confirmed / superseded / unknown | 業務効果key、要求digest、owner receipt、対象版。受付だけでRunの反映目的を成功にしない |

取消は成功確定と同じstore直列化点で競合解決する。確定が先なら後続取消はClosed、取消が先なら確定不可。遅延結果は既存operationへ照合し、取消済みRunを成功に戻さない。親取消・期限は子へ伝播する。子受付はadmit_childの同じ原子操作内で親claim・共有budget・deadlineを検査し、親の上限を拡大しない。親のfenceと子作成の間に所有者交代・取消が割り込む窓はない。ChildRun操作には受付時点で子Run IDが確定し、send_countは0のままAcceptedとなる。非終端の子は親操作を未精算のまま待機させ、子の終端と費用精算・停止/cleanup確認後に親の受付操作をAppliedへ一回精算し、ChildRun slotを解放する。子の終端理由が失敗・取消し・結果不明でも成功へ読み替えない。親操作には呼出し時の固定specを保存し、精算後も同じ子のRunHandleを返す。既存の受付結果とjoin/reconcileは親の取消し・絶対期限後も現在claimとPolicyで照会可能であり、新規受付だけを拒否する。Waitingへの遷移では親の実行用Run slotを原子的に返すが、子の寿命slotと有料操作の未精算予約は返さない。

## 2aで具体化した更新条件

`Claim`のlease値は取得時の参照値。更新時はstore上のworker/generationと現行leaseを検査する。renewは同世代のleaseだけを延ばし、expired leaseの復活は認めない。renewはRun revisionを変更しないため、checkとcheckpointの間に同じownerのheartbeatが完了してもcheckpoint CASは競合しない。更新完了の証拠は返却Claimと再取得したownerのlease_untilであり、revision増分ではない。checkpointの保存はrevisionを進め、古い版の保存は引き続き拒否する。再取得は必ずgenerationを増やす。送信回数と再送証拠の世代もoperationに保存し、NotFound証拠を保存した直後のWorker停止でも、新ownerが再照会なしに送信できないようにする。

Sessionの先行する非終端Runがある間は新規業務を機械的待機とする。確定でcontext版を進め、旧版の後続Runは`CONTEXT_STALE`の非成功となる。呼出し側は現行contextで新しい入力Runを作る。終端Runの書換えや暗黙のcontext再baseをしない。

会話本文の保存、返信Run終了後の資源寿命、別processでの再入力復帰は
[chat hostの責務表と公開example](chat-host.md)を参照する。

未計測replyは`unsettled_result`と`usage_observations`へ保存し、operationの実行結果と精算をUnknownのまま保持する。候補が届いたことだけで料金を0にしない。保存時にfence済みの完全な結果、または再照会で完全なusageが得られた場合だけ元予約を精算する。保存観測の復旧と精算後の重複解除は[PostgreSQL設計](postgres.md)を参照する。

2aのFinalizerは保存済み結果を確認する純粋な信頼済み拡張であり、終端は一つのstore commitで確定する。外部Artifact seal/owner finalizeを行う段階ではFinalizing intentを先に保存し、元ID照会を伴う操作として実装する。enumのFinalizingがあることを、Artifact確定の実装済み証拠にはしない。

data schema 3は[workspace](workspace.md)・`data.rs`が正本。namespace状態の原子的commitにworkspace revision、file hash、artifact revision/保存状態、共有grant、検証evidence、completion ID、context revision/high-waterを保存する。Stagedは所有/期限付きで復元可能だが未公開、Savedのみ保存完了。未知/旧schemaは拒否する。localはnamespace別file lock＋rename/fsync、memoryはnamespace別mutex。保存済みArtifactとRun成功・業務反映は別状態。

## 2e：ExecutionLedger

ExecutionRecordの所有Run/operationと固定ExecutionRequest（Workspace snapshotと実byte列、runtimeを含む）はinsert後不変。revision CASで取消要求・最新観測・資源別条件/確認を保存する。Memory/Pgで同じadvance検査を使い、取消、既知receipt、実行状態の巻戻しや資源条件の欠落/変更を拒否する。

CPU/Memory/PIDは停止receipt、Disk/Volumeは各削除receipt、ExternalCostは費用receiptで解放する。DestroyedだけではVolumeや未知費用を解放しない。pending()は未回収行を返し、Pgでは新しいstoreから再発見できる。backend registry全喪失と台帳耐久性は別であり、ProcessBackendで識別できない実体はUnknownとして隔離する。詳細は[execution](execution.md)・[postgres](postgres.md)。
