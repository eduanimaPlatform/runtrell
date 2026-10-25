# 障害マトリクス

| 障害 | 受付/所有権 | intent前 | intent〜I/O | 結果〜checkpoint | 確定/移管 |
|---|---|---|---|---|---|
| Worker停止 | 同key再照会/lease再取得 | I/Oなし | 元operation lookup、無条件再送なし | 台帳結果をCPへ再適用 | receipt再照会、二重確定なし |
| 保存失敗 | 成功応答なし | I/Oなし | 送信barrier未完ならI/Oなし | 成功報告なし、lookup | CAS未commitなら成功なし |
| stream切断 | Run寿命と独立 | 部分引数は未実行 | usage未知を保持 | partialを候補採用しない | 通知再送はRun再開始なし |
| 外部応答喪失 | 元Runを返す | 該当なし | Unknown/予約保持 | 元ID照会で精算 | 受付と反映を分離 |
| 取消競合 | request保存 | 新規業務禁止 | 新規送信禁止、既存lookup継続 | 結果/費用記録継続 | 先にcommitした取消/確定を採用 |
| 期限超過 | 理由付き終端 | 新規I/O禁止 | lookup/cleanup継続 | 未確定費用保持 | 新規確定禁止、既存receipt照合 |
| 古いowner | generation拒否 | 拒否 | Lookup後も再検査して送信拒否 | 遅延結果は現owner照合へ | CP/確定/移管拒否 |
| 重複開始/event | key+digest一致だけ元Run | 同operation IDを使用 | 同attempt・予約、host効果key | 同結果のみno-op、異値Conflict | completion IDを再照会 |

compaction失敗は現行CP維持。同時入力後の古い要約はrevision CASで拒否し、新入力を消さない。取消、停止確認、回収確認、精算は別記録。unknownをtimeoutで消す処理はない。外部NotFoundのscope/保証期限がない場合もUnknownで保持する。

2b: exact-replaceの0件/複数/古いdigest/binary/非UTF-8/空一致/上限は理由付き拒否。複数file/同一fileの複数編集は全件成功時のみ1commit。同一fileの各expected_digestは開始時の内容を照合し、old_text置換はstaging上で順番に行う。local stateのsymlink/hardlink/非regularや保存失敗で成功応答を返さない。圧縮は開始ContextState全体とrevision/high-waterを比較し、途中append・不完全reply・tool callのあるsummaryは保存しない。保存受付のStagedを再openでSavedと報告しない。Artifact確定後の差替えや別completionへの再sealを拒否する。
