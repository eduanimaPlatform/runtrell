# Runtrellの必要時実行（2e）

## 目的と受入境界

Run受付・検索だけでは環境を作らない。コード要求は既存dispatchのSandbox操作として、階層予算と共有slotを先に予約する。RunStore / OperationStore / BudgetAuthority / Schedulerは変更しない。新しいExecutionLedgerは取得intent、固定入力、取消、停止、資源別回収、費用を別に保存する。Memory adapterは共有instance内のみ、PgStoreのExecutionLedgerはDBへのCOMMIT後の耐久保存を提供する。台帳の耐久性とbackend実体の再発見能力は別契約とする。

取得はoperation由来の安定keyで冪等。取得応答を失ったときは同keyで発見する。取消後の遅延取得も実行せず破棄する。未確認の取得は無資源と推定しない。実行結果と停止・回収・費用確定を分離し、全解放条件がそろうまでSandbox操作をAccepted/Unknownとして維持する。古いWorkspace copyの回収はDataStore.mergeの入力snapshot CASを使う。回収失敗は出力を保持し、古い版を上書きしない。

## 初期backendの選定

信頼済みの小さな計算・構造変換は通常の信頼済みRust Toolを共通dispatch経由で呼ぶ。独自DSL・独自命令系は作らない（KISS/YAGNI）。2dのRegisterBackend、Program::Registers、Instruction、evaluate、fuelは意図した仕様変更として撤去した。処理本体のCPU/RAM・強制停止を共有process内で保証できない要求には、適合する隔離backendを使うか開始を拒否する。

ライフサイクル試験はruntrell-testkitのFakeExecutionBackendへ移した。requestのprogramを評価せず、取得応答喪失・取得前後の遅延・実行中保持・cleanup失敗・未知費用を制御する試験専用実装であり、本番backendとして提供しない。

不信コードはprocess backendへ渡す。macOSはsandbox-execのdeny-default profileで、runtime read-only・専用scratchだけwrite可・networkなし・forkなし。Linuxはbubblewrapのuser/mount/PID/network namespace、専用scratchと明示read-only runtime、die-with-parentを要求する。外部ツールはhost設定で渡し自動installしない。利用できない場合は開始拒否する。コンテナ/VM/cloudは同じ契約の別実装にできる。

## 強制能力（約束する単位）

| 制限 | macOS process | Linux process |
|---|---|---|
| 隔離 | Seatbelt deny-default。dyld起動に必要な `(literal "/")` の読取りを許可し、rootディレクトリ一覧が読める。子pathへの再帰読取り許可ではない | bubblewrap namespace。利用不能なら拒否 |
| CPU | helperのRLIMIT_CPU、fork拒否時のみ | aggregate CPU非対応、要求時拒否 |
| RAM | RLIMIT_ASは有効でないため拒否 | tree全体のRAM上限は非対応・拒否 |
| PID | fork拒否、対象1 process | quota非対応・要求時拒否 |
| disk総量 | 総量quota非対応・要求時拒否 | 総量quota非対応・要求時拒否 |
| 個別file | 非対応・要求時拒否。appendで上限越えを実測 | helperのRLIMIT_FSIZE（Linux実試験未実行） |
| wall時間 | 監視taskがkill/wait。OS停止中の実時間保証なし | 同左、namespace終了で子孫停止 |
| 出力 | stdout/stderr/回収fileへ各1/3のbyte枠。超過でkill/回収拒否 | 同左 |
| network | 全network拒否 | network namespaceで外部接続なし |

wall timeoutをhost scheduler停止も含む厳密な実時間保証として扱わない。processの起動に失敗した場合もhost実行へfallbackしない。総量CPU/RAM/PID/diskが必須のLinux jobはcgroup/quota対応backendが必要であり、この段階のprocess backendでは拒否する。

## 資源台帳と資格情報

CPU・Memory・PIDは停止確認、Disk・Volumeは削除確認、ExternalCostは費用receiptが解放条件。Future drop、期限到達、Run取消だけでは解放しない。環境はRun間再割当しない。backend状態と台帳は別々に照会でき、cleanup途中停止後の再照合は冪等にする。

setup/依存取得/installerも通常実行と同じ要求・予約・期限・network条件に従う。初期backendはnetwork denyのみ、資格情報受渡しなし、stdin null、TTYなし。対話が必要という宣言を拒否し、入力を無視して待つprogramはwall timeoutで終了する。親envは全消去し、HOME/TMPDIR/PATHを専用値へ固定する。共有cacheは提供しない。全tenant secretや管理者secretを渡すAPIは設けない。

## 使用順序と状態照会

1. hostが認証済みAccessで`freeze_workspace`を呼び、全fileを版付きreadする。最後にsnapshot一致を再検査し、入力copy全体を固定する。
2. `Runtime.execute_backend`がSandbox intent、全祖先budget、共有slotを予約し、ExecutionLedgerへ安定operation IDの取得intentを保存してから取得する。
3. 取得後にRunの取消・期限・owner・現行policyを再検査してstartする。取得応答を失った場合は同じIDの環境だけを照会し、未実行のReadyならfence後に元環境をstartする。置換環境の取得はしない。
4. process supervisorは呼出しFutureとは独立して監視する。Running → Collecting（実行停止済み、出力保存待ち）→ Stopped → Reclaiming → Destroyed。Collecting/回収中も停止とdisk解放を混同しない。コード・runtimeの版はrequestとhelper/launcher/実行file allowlistのhashで固定する。実行fileはhost allowlistに限定し、開始直前にもhashを照合する。
5. 同じrequestで`execute_backend`を再度呼ぶと元operationのlookup/cleanup/精算を行う。全資源の条件が成立するまでAccepted/Unknownでslotと費用予約を保持する。`ExecutionLedger.pending`で管理Workerが未回収行を発見する。
6. process出力fileは論理入力fileと`output_paths`の明示集合だけを回収する。NOFOLLOWで各componentを開き、regular file・nlink=1・byte枠を検査し、削除もmanifestへ記録する。保存済みcopyは`merge_process_copy`、任意のhost生成変更は`merge_copy`でCAS反映する。Executionの完了はWorkspace保存やRun成功のreceiptの代わりにしない。

初期backendはReadyで実行を待つが、pool/idle再利用はしない。1 operationにつき1 execution、終了後に破棄する。取消には`Runtime.cancel_backend`を使い、通常の`Runtime.cancel`だけではSandboxの実停止を確認したことにしない。

取得・停止要求・cleanup照会には`acquire_ms`/`stop_ms`/`cleanup_ms`を設ける。取得は元Run deadlineでも打ち切る。timeoutはUnknownを返すだけで解放しない。再照合の管理呼出しは続けられるが、新しい業務実行の予算や元Run期限を延長しない。同期filesystem syscallやhost schedulerの停止をこれらのFuture timeoutで強制中断できるとは主張しない。厳密なIO停止期限は初期backendの保証外。

ProcessBackendの共有mutexは短い状態更新だけに使い、copy・command hash検査・出力回収・directory削除中には保持しない。他Runの取消を一つのfile処理で全体直列化しない。

## このhostで判明した制約

Codex sandbox内ではSeatbeltの入れ子適用が`sandbox_apply: Operation not permitted`で拒否されるため、実隔離試験はignoreを維持する。ユーザー提供のClaudeレビューでは、sandbox外で`cargo test -p runtrell --test execution -- --ignored`が成功済み。本2e実行での再実行結果ではない。Linux実隔離・子孫停止は未検証。

RLIMIT_FSIZE=4096の単発8192-byte書込みは4096でSIGXFSZとなったが、64-byte appendを512回行う有限probeは32768 bytes・exit 0になった。そのためmacOS file上限は非対応として拒否する。CPU=1秒のbusy-loopは別の5秒wall監視付き試験で停止した。

ExecutionLedgerはMemoryExecutionLedgerとPgStoreが実装する。Pgの保存schema・transaction境界は[postgres](postgres.md)を参照。所有Run・operation・固定request（runtime/Workspace入力を含む）・revision・取消要求・最新観測・資源別解放条件とreceiptを保存し、pending()は未回収行だけを返す。CASはrequest/条件の変更、取消・receiptの巻戻し、異なる実体の観測、古いrevisionを拒否する。

## backendの再照会契約

新しいbackend clientは保存済みowner/operation/runtime版を照会keyとし、元の実体と要求を検証できる場合だけ再照合する。照会方式・実体世代・権限・receiptの根拠はbackend別の契約である。lookupのNoneや通信断を、不在・停止・削除のreceiptに読み替えない。回復中に置換環境をacquireせず、削除未確認の実体を別Runへ再割当しない。

FakeExecutionBackendの別clientは独立した外部inventory役のFakeExecutionRegistryを共有し、管理Worker/backend client再生成後の回収を試験する。registry全喪失は別試験でUnknown保持を確認する。Memory ledgerをcloneする回復はprocess停止耐性の証拠ではない。Pgは新しいPgStoreからpending()で再発見できるが、backend registryの耐久性を追加するものではない。

ProcessBackendのregistryはmemoryのみ。全喪失後は安全な実体再識別を行えずUnknownとして隔離する。残存directoryを再取得時に見つけても採用・削除・別Run再割当しない。PIDだけの採用はPID再利用を区別できないので行わない。耐久的な実体世代・所有証拠を伴うprocess再発見は残課題。Tokio runtimeはsupervisorが停止・回収を終えるまでhostが維持する必要がある。

helperは新しい単一thread processで`/dev/fd`（macOS）/`/proc/self/fd`（Linux）の番号だけを列挙し、stdio以外の継承FDをcloseしてから制限・隔離を適用する。親が非CLOEXECで開いた合成credential FDを渡すRed→Green試験で遮断を確認する。unsafeはこのhelper内のraw FD closeに限定し、fork後のpre_execへRust処理を入れない。credentialの内容を読んだり渡したりしない。

copy・hash検査・出力file回収・物理directory削除はTokioのblocking workerへ出し、回収task自身が結果をregistryへ保存する。呼出しFutureがtimeout/dropしても、完了した資源を後続lookupで発見できる。出力回収待ちで停止receiptの更新を待たせない。

`Observation.released_resources`は資源ごとの確認receiptである。`Destroyed`だけではdisk/永続volumeを解放しない。外部backendが環境nodeを削除してもvolumeが残る場合は、Volumeのreceiptを欠いたまま保持し、同じdestroy/lookupを再試行できる。初期local backendは永続volumeを割り当てず、scratch削除時にDiskとVolumeの確認を明示する。未確認volumeと未確定費用を、環境状態enumから一括解放しない試験を追加した。

出力fileのpath/manifest費用もbyte枠へ含め、変更のない入力fileはstream比較して出力から省く。stdout/stderr/fileを合わせた最終JSON envelopeにもoutput_bytes上限を適用する。runtime hash不一致等の開始前拒否は未使用環境をStoppedとして記録し、後続cleanupで回収できる。
