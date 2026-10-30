# 実行境界（基準設計§7.2）

| 手順/barrier | 先に保存するもの | 成功応答 / 復旧 |
|---|---|---|
| 1 受付 | request key+digest、実効設定、期限、session入力順 | commit後のみ受付。重複は元Run、別payloadはConflict |
| 2 所有権移管 | generation+lease、Run revision | commit後claim返却、その後get。古い読取を再利用しない |
| 3 照合 | 未完了operation・予約・失効・既存結果 | 新規判断前にlookup。結果不明を未実行とみなさない |
| 4 実行許可 | 現在policy、descriptor/account版、期限、共有枠 | 不許可は新規I/Oなし。model/tool/compaction/子/retry共通 |
| 5 I/O前intent | request全体、ID/digest、attempt、価格、全階層予約 | begin commit後のみmark_sent。mark_sent commit後のみI/O。送信と外部の原子transactionは仮定しない |
| 6 結果記録 | result/external ID/usage、精算、slot状態 | 同値重複のみno-op。保存失敗は再送せずlookup。unknownは予約/slot保持 |
| 7 checkpoint | protocol状態、公開履歴、operation cursor、context版 | CAS commit後次判断。結果保存済み/CP未保存なら台帳結果を再適用 |
| 8 確定 | 検証hash/入力版/設定版、安定completion ID、Run終端 | Finalizer後CAS。Artifact seal、Run、業務反映・配送は別receipt |

NotFound再送は、同じscope・retention内の未実行保証がある場合だけ。同じrequest bytes、output cap、attempt、予約を再利用する。Lookup後に所有権・取消・deadline・認可を再検査する。古いowner更新はOwnershipLost。遅延外部応答は破棄せず現ownerのlookupへ収束させる。任意外部サービスのexactly-onceは保証しない。別Run間の効果key/digestは業務ownerが冪等処理し、runtimeの操作台帳とは別に検証する。

取消・期限後は新規送信を止める。lookup/record/settlement/cleanupは業務開始と別の継続経路。停止不能・未知料金・残diskは終端Runと独立して未完了を残す。保存失敗の注入はtransaction適用前に失敗させ、部分更新を残さない。MemoryStoreのclone共有はworker再生成を試験するだけでOS再起動を試験しない。

2b: copyの入力はWorkspaceSnapshot全体で固定する。回収mergeは現行snapshot完全一致と各file期待revisionの双方を検査し、write/deleteを1回commitする。保存済みデータを古いcopyで上書きしない。2dは[ExecutionBackend](execution.md)を追加。OS process backendと試験用FakeExecutionBackendを提供し、実隔離確認には実行環境制約がある。Artifact確定は現在のmanifest/hashとstore保存evidence・必要validator版をnamespace transactionで確認する。DataFinalizerのcompletion保存後、2aのRunStore.finalizeがRun終端を別に保存する。安定IDで再照会でき、業務owner反映の代わりにはしない。

## 検証不合格と修正（rt-3c）

検証の失敗結果は`passed=false`のreceiptと構造化diagnostics。現在の `Validator` trait に `repairable` method はない。host は各 `Diagnostic.repair` を `DiagnosticRepair::{Agent,Host,Operator,Unknown}` で明示し、`ValidationReceipt.repairable` が true かつ全 blocking 診断が Agent の場合だけ修正可能とする。Host/Operator/Unknown を receipt flag、code、path だけで Agent へ昇格させない。info/warning 以外の未知 severity も修復不能。権限拒否・保存障害・receiptのhash/根拠版/構成版/検証器版不一致はErrorであり修正候補にしない。driverは最終候補でもreceiptを照合してから分類する。

修正可能なら、候補・失敗receipt・開始時刻・修正回数・次のprotocol状態・モデル入力へ追加する診断を一つのcheckpoint CASで保存してmachine waitに移る。実行slotは解放し、hostの通常の再pollで再開する。checkpoint失敗時は追加モデルを呼ばない。Worker再起動でも同じ修正番号のoperation keyを用い、送信済み呼出しは通常のlookup経路で照合する。修正結果は毎回Validatorへ戻し、古いreceiptを転用しない。

`RepairPolicy.max_repairs` と `max_elapsed` は host が供給する任意上限で、既定はどちらも None。これとは別に `max_no_progress: NonZeroU64` は必須で既定3。失敗 fingerprint ごとの出現数を checkpoint に保持し、A→B→A→B→A のような再出現でも3回目で `VALIDATION_NO_PROGRESS` に停止する。総修正上限なしを無進展停止なしと解釈しない。詳細は [API](api.md)。Runの予算・期限・所有権・認可は常時適用する。経過時間上限は次のpoll開始時にも照合し、修正モデル呼出しのtimeoutにも反映する。候補での上限到達/修正不能は最新候補とreceiptを`last_validation`へ保存して理由付きFailed、予算/Run期限は既存の終端理由へ収束する。無料確定を許す口座でも追加LLMは`dispatch`の通常予約対象。保存障害・証跡不整合はErrorとしてhostへ返し、成功や修正待ちへ変換しない。

予約拒否Waitはstateを保存してdriveを返す。待機中にDB transaction・lock・実行slotを保持しない。hostは期限内に再pollし、同じ許可検査/全祖先予約を繰り返す。Shrinkは`max(接続先min_max_output_tokens, host min_output_budget_tokens)`を満たす予約枠だけを採用する。
