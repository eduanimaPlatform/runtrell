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
