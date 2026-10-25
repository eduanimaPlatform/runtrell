# Runtrellの必要時実行（2e）

## 目的と受入境界

Run受付・検索だけでは環境を作らない。コード要求は既存dispatchのSandbox操作として、階層予算と共有slotを先に予約する。RunStore / OperationStore / BudgetAuthority / Schedulerは変更しない。新しいExecutionLedgerは取得intent、固定入力、取消、停止、資源別回収、費用を別に保存する。Memory adapterは共有instance内のみ、PgStoreのExecutionLedgerはDBへのCOMMIT後の耐久保存を提供する。台帳の耐久性とbackend実体の再発見能力は別契約とする。

取得はoperation由来の安定keyで冪等。取得応答を失ったときは同keyで発見する。取消後の遅延取得も実行せず破棄する。未確認の取得は無資源と推定しない。実行結果と停止・回収・費用確定を分離し、全解放条件がそろうまでSandbox操作をAccepted/Unknownとして維持する。古いWorkspace copyの回収はDataStore.mergeの入力snapshot CASを使う。回収失敗は出力を保持し、古い版を上書きしない。

## 初期backendの選定

信頼済みの小さな計算・構造変換は通常の信頼済みRust Toolを共通dispatch経由で呼ぶ。独自DSL・独自命令系は作らない（KISS/YAGNI）。2dのRegisterBackend、Program::Registers、Instruction、evaluate、fuelは意図した仕様変更として撤去した。処理本体のCPU/RAM・強制停止を共有process内で保証できない要求には、適合する隔離backendを使うか開始を拒否する。

ライフサイクル試験はruntrell-testkitのFakeExecutionBackendへ移した。requestのprogramを評価せず、取得応答喪失・取得前後の遅延・実行中保持・cleanup失敗・未知費用を制御する試験専用実装であり、本番backendとして提供しない。

不信コードはprocess backendへ渡す。macOSはsandbox-execのdeny-default profileで、runtime read-only・専用scratchだけwrite可・networkなし・forkなし。Linuxはbubblewrapのuser/mount/PID/network namespace、専用scratchと明示read-only runtime、die-with-parentを要求する。外部ツールはhost設定で渡し自動installしない。利用できない場合は開始拒否する。コンテナ/VM/cloudは同じ契約の別実装にできる。

## 強制能力（約束する単位）

| 制限 | macOS ProcessBackend | Linux ProcessBackend（従来の限定構成） |
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

CPU・PIDは停止確認、Memoryはbackendの実メモリ回収確認（Linux tmpfsではunmountとcgroup削除）、Disk・Volumeは削除確認、ExternalCostは費用receiptが解放条件。Future drop、期限到達、Run取消だけでは解放しない。環境はRun間再割当しない。backend状態と台帳は別々に照会でき、cleanup途中停止後の再照合は冪等にする。

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

## rt-3b: Linux の主力構成（実装済み・Linux 実行未検証）

公開の任意 backend `LinuxBackend` は既存の ExecutionBackend / ExecutionLedger / lazy dispatch を利用する。既存 `ProcessBackend` と macOS 経路は残す。上表の Linux process の非対応欄は従来の ProcessBackend の能力であり、LinuxBackend の制約は以下が正本となる。全 Run への起動や採用を強制しない。ホストは具体的な資源値と実行許可を供給する。

- Linux cgroup v2、`cpu memory pids` が有効な専用委譲 subtree、書込み権限、`cgroup.kill`、`memory.peak`、bubblewrap の user/mount/PID/network/IPC/UTS namespace、host mount namespace の CAP_SYS_ADMIN を必要とする。いずれかを利用できなければ拒否し、host 実行への fallback はない。実行要求には CPU・RAM・PID・disk の全上限を必須とする。能力不足が取得途中で判明した場合も Unknown の資源予約を残す。
- host が constructor の `LinuxCpuBandwidth { quota_usec, period_usec }` で供給する値を `cpu.max` に設定する。試験構成は `10000 10000` で group 全体の帯域を 1 CPU に制限する。`cpu_millis` は `cpu.stat usage_usec` の累積監視閾値で、10ms ごとの supervisor が超過時に停止する。**累積 CPU の厳密な無超過保証ではない**。帯域制限は kernel が強制するが、累積閾値と wall watchdog には supervisor の scheduling 遅延による超過がある。厳密な累積 CPU 上限が必要な host はこの backend を選択しない。CPU 使用量には入力 staging・launcher・全子孫を含む。
- `memory.max`、`memory.swap.max=0`、`memory.oom.group=1`、`pids.max` を設定し、値を再読取りする。`memory.peak`、`memory.events oom_kill`、`pids.events max` を観測し、停止後も管理記録に残す。kernel の memory accounting 単位・一時的な reclaim の遅延があり、RSS の単純な総和とは異なる。kernel memory と tmpfs の計上は cgroup の仕様に従う。
- scratch は host が `size=disk_bytes,nr_inodes=4096,mode=0700,nodev,nosuid` で mount した tmpfs。入力の書込みも group に入った Rust helper で行い、tmpfs page と staging を資源管理対象にする。`/workspace` のみ書込み可能、`/tmp` はその symlink。namespace root と runtime は read-only、`/dev` は必要な device の個別 bind のみ。追加の書込み可能 tmpfs や host `/etc`・home・cgroup・管理領域を渡さない。disk 上限は **scratch の page 割当**の上限であり、metadata、管理記録、保存先 Workspace、stdout/stderr の host メモリを含む全ホスト disk quota ではない。`disk_bytes` は host の page size の整数倍だけを受理し、丸上げで要求を超えない。小さな書込みは page 単位、inode 数も有限、RAM と disk の双方で拒否され得る。管理記録・入力／出力 envelope の host 側余裕は別途確保する（入力16MiB、出力16MiB以下の既存契約）。
- `--unshare-all --disable-userns --cap-drop ALL` により追加 user namespace で read-only mount を作り直す経路も閉じる。外部 network と資格情報は渡さない。`/usr`・`/bin`・`/lib`・`/lib64` は host が用意した immutable runtime tree として read-only 公開する。この tree に秘密情報を置かない。allowlist にない executable を host で起動しないが、sandbox 内の runtime tree の実行 file を不信コードが呼ぶことは可能であり、制限は tree 全体に適用される。
- `file_bytes` は helper の RLIMIT_FSIZE、stdout/stderr と成果物は既存の byte 枠、NOFOLLOW・regular file・nlink 検査を再利用する。helper/launcher/実行 file の hash を開始時に再検査する。任意の `ExecutionRequest.executable_hash` も backend の固定 hash と照合する。host は実行中の runtime tree を不変に保つ。共有 kernel の脆弱性に対する VM 相当の境界ではない。

### registry に依存しない再発見と回収

`root/<SHA256(owner,operation)>/` に scratch・cancel・lock・`record.json`、委譲先に `rt-<同ID>` を作る。root は canonical・所有者一致・0700 のローカル耐久 FS、同じ mount namespace から再接続できることが前提。record は固定 request、owner/operation/runtime 版、cgroup inode、scratch mount ID、状態、結果、使用量、receipt を持ち、temp file→fsync→rename→directory fsync で保存する。管理記録は sandbox に見せない。

helper は親 PID の前後照合と parent-death SIGKILL を設定する。cgroup 参加時には専用 join lock を取り、耐久 cancel marker があれば参加を拒否する。停止側も marker を保存して同じ barrier を通過してから kill／空判定するため、再起動後の遅延 helper が停止確認後に参加する経路を閉じる。参加前の信頼済み helper 起動分は host の管理余裕であり、不信 payload は参加完了まで実行しない。

開始前に Running を保存し、実行中は operation 単位の OS file lock を保持する。別 client は atomic record を照会し、耐久 cancel marker で取消できる。取得中に staging が operation lock を保持している場合も、停止側は識別済み group を kill できる。取得側の record 更新と競合させず Pending を返し、後続の停止確認で receipt を保存する。backend process が失われると OS が lock を解放する。Running を Ready に戻したり、同じ操作を再実行したりしない。scratch と cgroup の途中作成で識別証拠を保存できなかった窓は、安全を優先して Unknown として残る。

管理 Worker は通常の owner/lease fence と管理権限を取得したうえで、`ExecutionLedger.pending()` と `inventory()` の owner/operation/runtime を照合し、`reconcile(&pending_record)` で固定 request 全体も一致させる。元の ToolContext の owner/operation で `stop` を繰り返し、`cgroup.kill` と `cgroup.events populated 0` の確認後だけ CPU/PID の receipt を得る。tmpfsは停止後もpageを保持するので、この時点ではMemoryを解放しない。PID 単体を識別子にしない。復旧時の終了理由・stdout/stderr は再現できないため `RECOVERED_STOP_RESULT_UNKNOWN` とし、元 mount が一致する場合だけ bounded な file copy を保存する。これは正常終了の receipt ではない。

`destroy` は停止確認後に Reclaiming を耐久保存し、通常の unmount（lazy detach 不使用）・mount IDが消えたこと・scratch directory 削除・cgroup 削除の確認後に Memory/Disk/Volume を解放する。途中停止後も再試行できる。Destroyed の管理記録は tombstone として保持し、同 operation の acquire は元 reference を返す。記録の削除／retention は、その operation を二度と再受付しない host 側方針と合わせて別途実施する。

記録の破損、別 runtime 版、request 不一致、cgroup inode／mount ID 不一致、対応記録のない directory/group は Unknown として隔離する。自動で削除・別 Run に再割当しない。`inventory().execution == None` は隔離対象。名前の一致だけで採用しない。ホスト再起動、管理 FS 喪失、mount namespace 変更は通常の backend process 再起動と異なり、存在／削除の receipt を再構築できない場合がある。ローカル record は ExecutionLedger の原子予約・owner fence・費用精算を置き換えない。ローカル実行の外部課金は0であり、host/cloud 利用料の課金 adapter を実装したという意味ではない。

### 任意の Python 実行（既定無効）

`runtrell` の `python` feature は request builder `execution::python::{PythonRuntime,DependencyPolicy}` を公開する。管理基盤は Rust、実行する Python は host 提供の別 process である。Python interpreter の install や全 Run への Python 起動を行わない。

host は固定絶対 path、正確な `major.minor.patch`、実行 file SHA-256 を供給する。builder と backend の双方で hash を検査し、sandbox 内で版を照合してから `-I -B` で script を実行する。初期構成では host が `/usr` の read-only runtime tree に interpreter・標準 library・必要 package を事前準備する。tree 外の単体実行 file も read-only bind できるが、任意の venv／外部 library tree の自動検出・取得・mount は行わない。

`freeze_workspace` で固定した `input` と `input_files` を `PythonRuntime.request((snapshot, files), script, output_paths, limits, PreparedOnly)` へ渡し、通常の `Runtime.execute_backend` を呼ぶ。`merge_process_copy` が元 snapshot の CAS で保存を行う。Python 成功、成果物回収、Workspace 保存完了、停止、削除は別 receipt である。保存競合を成功にせず、出力 copy は管理 record に保持する。

`PreparedOnly` が既定で network deny、資格情報なし、事前に用意した package／環境だけを使う。wheel の取得・pip install は実装しない。`Fetch { network_allow, credential_scopes, budget_micro_usd, deadline_ms }` は将来の方針型であり、現実装は必ず `PYTHON_DEPENDENCY_FETCH` で拒否する。後続実装にも通常の予約・network・資格情報・資源・期限・費用管理を適用する。

### GitHub-hosted CIの受入境界

- state root（`RUNTRELL_LINUX_STATE_ROOT`）とその全祖先directoryは、管理helperのuidが **CAP_DAC_OVERRIDEなしで** 辿れる必要がある。bubblewrapはpayload用のuser namespaceへ移った後にscratchのsource pathを解決するため、ownerの異なるhome配下（例：GitHub-hosted runnerの `/home/runner`、mode 0750）に置くと `Can't find source path ... Permission denied` で起動できない（2026-10-25の初回Hosted CIで観測）。CIでは `/var/lib/runtrell-ci-state`（root所有、0700）を使う。本番でも同じ条件を満たす場所に置く。

[公開CI](../../.github/workflows/ci.yml)は標準`ubuntu-24.04` VMの独立した実隔離jobを使う。通常の契約jobは使い捨てPostgreSQL 18 serviceを使い、Python feature有無でunit/integration/doctest・公開example・trace/hashを確認する。macOSは標準`macos-latest`上のSeatbelt実隔離jobを使う。Self-hosted、larger runner、有料cache拡張、private接続・秘密情報は使わない。

Linuxの管理側はsudoで専用`/sys/fs/cgroup/runtrell-ci` subtreeを作り、親と専用subtreeの`cpu memory pids` controllerを有効化する。専用subtreeには管理processを置かず、backendがoperation単位の子groupへhelperを参加させる。LinuxBackendを保持する管理supervisorにはhost mount namespaceのCAP_SYS_ADMINが必要で、backend自身が要求disk枠のtmpfsをmount/unmountする。runtrell-exec-limitはその子として起動し、cgroup参加・入力staging・rlimit・bubblewrapへのexecを担当する。Rustのbuildは通常userで行い、実試験の管理処理だけsudoで実行する。

payloadの権限は管理側と別。bubblewrapの`--unshare-user`を必須とし、uid/gid 65534、`--cap-drop ALL`、helperの`no_new_privs`、追加userns禁止、read-only runtime、network分離を適用する。管理領域・資格情報・cgroup FSは公開しない。`linux_payload_has_no_privileges`はpayloadの実行、uid/gid、全5種のcapability集合が0、NoNewPrivs=1、管理側と異なるuser namespaceを照合する。共有kernelの脆弱性に対するVM相当の防御を意味しない。

`xtask isolation-tests linux default|python`が必須名の正本。現在default 7件、Python有効8件（既存の環境拒否・遅延helper拒否2件を含む）。CIは各試験を`--exact --include-ignored --test-threads=1`で独立実行し、一覧と名前ごとのlibtest `ok`、1 passed / 0 failed / 0 ignoredを`xtask isolation-results`で照合する。必須名の欠落・追加未登録・skip・失敗・空の実行結果を拒否する。Crash試験の子processの不完全な出力は通常のlibtest capture内に保持され、`CANCELLED_BEFORE_JOIN`等のstderrを成功試験として数えない。異なるfeatureの件数も混ぜない。macOSは`cargo test -p runtrell --test execution -- --ignored`でSeatbelt 1件を同様に照合する。

既存のbaseline実行・CPU閾値・OOM・PID quota・ENOSPC・出力上限・network/書込み拒否・子孫停止・別process停止後の再発見・冪等回収・Python成果物保存のassertionは維持する。環境変数不足は成功returnせず失敗する。CIの結果照合だけでは将来追加される早期returnを検出できないため、payload実行と制限発動のassertionも必須条件である。

本改訂のHosted runは未実行・未確認。過去のHosted成功・失敗は対応する固定commitの証拠であり、この未commit差分の成功へ読み替えない。本macOS sandboxではLinux実隔離を実行せず、Docker操作もしない。Seatbeltは入れ子sandboxの適用拒否により実隔離をローカル検証できない。修正後はHostedの各必須名と結果を確認する。

Hosted成功後も、本番構成のkernel・immutable runtime・委譲設定・tmpfs権限・耐久管理FS・mount namespace・別管理processからの回収、実provider usage/費用との整合は別途受入が必要。DB kill・電源断・failover・長時間負荷も未検証。公開CI成功は利用側の統合受入や有料操作の実行許可を意味しない。


### Linux本番管理processと受入条件

現実装の信頼境界はLinuxBackendを組み込んだ管理supervisorと、その子の`runtrell-exec-limit`・bubblewrapである。mountをhelperへRPC委譲する機構は実装していない。supervisorを非特権の通常アプリと同じprocessへ組み込んで、cgroup委譲だけで動くとは扱わない。hostが用意する管理binaryをsystemd unitのExecStartから起動し、cgroup root・state root・helper/launcherの固定path、認証済みhost要求とexecutable allowlist・上限値を与える。管理supervisorがhelperを子processとして起動し、payloadはhelperを管理口として起動しない。helperをsetuid化せず、payloadから管理サービス・helperの管理引数・record/cgroup rootへアクセスできない配置にする。

本番ではsystemd等で専用サービスidentity、管理state directory（0700）、`Delegate=cpu memory pids`の専用cgroup subtreeを用意し、supervisorをoperation子groupへ入れない。管理サービスにはmount/unmountを行うnamespace内のCAP_SYS_ADMINと、選択したbubblewrap・uid/gid mapping・signal・ファイル所有権操作に必要な権限だけを与える。CapabilityBoundingSet等で上限を設け、秘密情報・ネットワーク・他サービスの管理権限を与えない。CAP_SYS_ADMINだけで全配布環境が成立することは未実証であり、サービスidentity・kernel・bubblewrap版に合わせて必要権限を検証する。`PrivateMounts`等でmount namespaceを分ける場合、復旧管理processも同じnamespaceへ接続しなければならない。state rootはbubblewrapが権限を落とした後も辿れる管理配置に置く。

payloadにはuser namespaceを必須とし、uid/gid 65534、capability全集合0、no_new_privs、追加userns禁止、network分離とread-only runtimeを適用する。host mount管理権限はpayloadへ渡さない。privileged DockerやsudoのHosted VMで成功しても、最小権限の本番systemd構成が実証されたことにはならない。この変更のLinux実試験は未実行で、管理側の実行担当が次を確認する。

- `cargo test -p runtrell --test linux_execution --no-run`（Python構成は`--features python`も付ける）。
- cgroup v2・host mount権限・userns・immutable runtimeを備えた専用環境で、`RUNTRELL_LINUX_TEST=1`、`RUNTRELL_CGROUP_ROOT`、`RUNTRELL_LINUX_STATE_ROOT`を設定する。
- `cargo run -p xtask -- isolation-tests linux default`（またはpython）が返す各試験を`--exact NAME --include-ignored --test-threads=1`で実行する。`stopped`はCPU/PID解放とMemory/Disk未解放を、`destroyed`はunmount・cgroup削除後のMemory/Disk解放を検査する。全回収まで待つcontroller契約は維持する。
- `linux_concurrent_runs_reclaim_independently`で8 Runの並行実行・回収を確認する。cgroup/mount作成直後かつinode/mount ID保存前のcrashは、まだ自動再発見・回収を提供しない。該当実体はUnknownで隔離し、予約を保持する。REC-05/06・SBX-07の未完了受入項目であり、通常のrecord保存後のcrash試験で代用しない。

### macOSの子孫実行能力

Seatbeltのfork拒否は維持する。`ExecutionRequest.requires_descendants=true`を指定した要求は、macOS ProcessBackendで取得前にUnsupportedとなる。macOSの子process一括回収は提供しない。必須CI試験`real_isolation_loop_output_fork_env_and_workspace_boundaries`は、この明示拒否と環境未取得を確認し、子孫回収の成功を主張しない。同じ試験のLinux fork経路はchild.pidの存在・数値PID・停止を必須にする。要求を宣言しないpayloadがforkしてもSeatbeltが拒否する。`requires_descendants`は隔離を緩める設定ではない。
