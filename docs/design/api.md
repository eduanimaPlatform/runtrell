# runtrell API（2a control schema 1 / data schema 3）

依存は host → `runtrell` → `runtrell-protocol`。fake host → `runtrell-testkit` → `runtrell`。新規コードはApache-2.0。crate名は実行機構、判断protocol、試験接続の責務を示す作業名で、registry名の取得を意味しない。protocolの型はruntime APIで再exportしない。

公開Rust型・traitの正本は `crates/runtrell/src/api.rs`。RunSpecはtenant、principal、重複request key、入力/目的、共有account、親、任意sessionと期待context版、根拠版、設定版、任意deadlineを固定する。RunHandleはtenant付きID。ToolContextは信頼済みdriverがclaimと固定descriptorから作る。model/toolが操作IDや権限を選び直さない。

RunStoreはstart/get/claim/checkpoint/cancel/terminate/finalize、OperationStoreはbegin/operation/mark_sent/record、BudgetAuthorityはcreate_account/accountを提供する。ControlStoreはこれらと共有slot・queue・警告配信を組み合わせる。beginは現在owner・取消・期限・accountを再検査し、全祖先accountの予約とintentを同じbarrierで保存する。recordは操作結果と精算を同じbarrierで保存する。分離adapterはこの原子性を実装せずにControlStoreを名乗れない。

PostgreSQL の外部 transaction 参加 API は `PgStore::{start_in, create_account_in, create_account_with_policy_in, terminate_in, finalize_in}`。host の SQL と同じ `sqlx::Transaction` に書き込み、commit は host が行う。親子関係と共有口座は RunSpec で指定する。保存・失敗時の契約は [PostgreSQL](postgres.md#host-owned-transaction) を参照。

async traitはRustライブラリの静的dispatch契約。安定dynamic ABIではない。ModelPort/Toolはinvokeとlookupを分離、lookupはFound/Pending/Unknown/NotFound（同scope・保存期限内で未実行を保証する証拠）を返す。timeoutやstream断はUnknown。provider内部retryを禁止し、課金retryは新attemptとして共通dispatchへ戻す。元request・予約を再使用するNotFound回復は同attemptの再送であり、新しい予算を作らない。

Policyは毎回現在の認可を検査する。SecretProviderは限定context・対象・期限でsecret利用closureを起動し、生secretをmodelや保存レコードへ返さない。Observerは機密payloadを含めない有限eventの受信口。Clockは単調な試験時刻（永続adapterでは共有epoch時刻）を与える。

Validatorは候補hash、根拠revision、設定/検証器版に結び付くValidationReceiptを返す。Finalizerは信頼済み業務拡張であり、外部反映済みreceiptを確認した安定completion IDを返す。RunStoreは現行revisionとreceiptをCAS照合する。別ownerへの業務書込みはoperationとしてdispatchし、その結果をFinalizerが確認する。Finalizer内で未記録の有料I/Oや副作用を実行してはならない。

WorkspaceStore / ArtifactStoreは2bでAccessを受け取る契約へ改訂し、[data.rs](../../crates/runtrell/src/data.rs)を型・traitの正本とする。本実装と制約は[workspace](workspace.md)参照。Workspaceはtenant/Run/Agent、版付き共有、digest/revision/manifest、CAS一括編集・exact-replace・copy回収を扱う。ArtifactはStaged / Saved / Finalizedを分け、範囲取得・外部出力・retention・削除確認を扱う。RunStore/OperationStore/BudgetAuthority/Schedulerの予算・受付方針は[budget](budget.md)を参照。ExecutionBackend・ExecutionLedgerの正本は[execution/mod.rs](../../crates/runtrell/src/execution/mod.rs)、能力とOS別制約は[execution](execution.md)。実行backendは能力/強制上限、acquire、start、lookup、cancel、stop、destroyを分け、ledger.pendingから回収を再照合する。必要制限非対応はUnsupported、host実行へfallbackしない。

RunSpec.budgetとscheduling、AccountPolicyはhostが供給する型付き・保存済みsnapshot。BudgetAuthority.create_account_with_policyで口座方針を固定する。create_accountは明示された既定方針の便宜API。finalization_allowedは無料確定だけの受付判定であり、RunStore.finalizeはowner・版・未精算操作を再検査する。使用量・価格の内数と画像交差、固定費操作、既知ゼロ/未計測の区別は[budget](budget.md)参照。recordはUsageUnknownを返す場合でも、不完全な結果と使用量の観測を保存して予約を保持する。Invalidは不正内数、Storageは保存失敗を表す。

wireはRust traitと別。schema=1、IDは文字列、金額は整数micro-USD、時刻は共有epoch tick、optional usageはnull（0と区別）。store checkpoint内のprotocol JSONは内部形式でhostの業務APIではない。未知schemaは拒否し、旧Runはdrainまたは理由付き終了。公開SDKのModelRequestは独自のMessage/ModelReplyのみを用い、protocolのserde表現をwire契約にしない。streamの部分deltaを直接実行せず、完全なreplyだけをdriverへ渡す。マルチモーダル/opaqueはMessageの版付きhost metadataへ保持するが、2a driverの実行対応はtext/toolに限定し、未対応は拒否する。

エラーはConflict（digest/版/CAS不一致）、OwnershipLost、Closed、Deadline、BudgetAtLimit、ReservationDenied、UsageUnknown、PermissionDenied、Unsupported、Backpressure、QueueTimeout、NotFound、Invalid、Storage。Storageは成功応答を返さず、I/O前失敗は外部呼出し0、I/O後失敗は元operation照合で回復する。

費用計算の公開APIは既存のUsage・Price・ModelLimitsへ統合する。Usage.total()は費用観測と独立した総量、Price.cost()は交差を含む区分別の実料金、Price.estimate(input_upper_bound, output_allowance)は最悪単価の予約上界を返す。ModelLimits.estimate()は接続先のcontext/出力指定範囲とmodel/価格一致も検査する。ModelLimits.min_max_output_tokensは技術的な指定下限、ReservationPolicy::Shrink.min_output_budget_tokensはhostの品質上の予約枠下限であり、実生成量の下限ではない。実際の価格・品質値・provider形式変換はhostが供給する。詳細は[budget](budget.md)。

## 任意の標準構成（rt-3c）

`assembly::RegisteredTool`は異なるTool実装を同じVecへ登録する。`ValidationChain`はSchemaと版付きValidatorを合成し、各receiptのhash/根拠版/構成版/検証器版を照合する。`CheckedFinalizer`は確定器が返すIDの存在と候補/receiptの完全一致を検査する。`AssemblyConfig::diagnostics`は名前衝突、必須validator不足、能力不足、範囲外設定、Schema構成不正を全件返す。具体的な接続先・業務presetはhostが供給する。`configuration_digest`は入れ子のobject keyをソートし、配列順を保持する。集合の配列化はhostが先に正規化する。自分自身のdigestフィールドは入力へ含めない。

`FileTool<S>`/`StoredOutputTool<T,S>`は`StandardToolStore`（WorkspaceStore + ArtifactStore）を、`DataValidator<V,S>`/`DataFinalizer<S>`は`EvidenceStore`を要求する。context圧縮は`ContextStore`を受け取る。既定型はDataStoreであり、外部adapterはtraitを直接実装するか、`NamespaceBackend`のopaque state読取/CASへ接続して共通アルゴリズムを再利用する。同期EvidenceStore/Validator/FinalizerはRuntimeとValidationToolが明示的にspawn_blockingへ配置する。Runtimeのvalidator/finalizerとValidationToolのadapterはArcで所有する。DataStore.validateもArc所有のTargetValidatorを受け取り、認可・対象照合・validator実行・証跡CASまでblocking poolで処理する。DataStoreのasync保存経路も同期namespace I/Oをblocking poolへ出す。native async adapterはWorkspaceStore等へ直接実装できる。同期APIの直接呼出しは呼出し元がblocking境界を担当する。

`schema::compile_schema`/`schema_diagnostics`/`validate_schema`は同一評価器。SchemaValidatorはTargetValidatorとして保存証跡へ接続でき、ValidationChainは構造化出力に使える。未知keywordを検査する前処理は対応範囲の拒否専用で、インスタンスの検証はjsonschemaへ委譲する。Schema構成エラーとインスタンス不合格は区別し、診断はinstance pointer（path）・schema pointer（schema_path）・原因を保持する。

## 予約前の復旧準備と証拠

`ModelPort`と外部job型`Tool`は`requires_recovery_preparation`を宣言した場合だけ、`prepare_recovery`で予約前に非同期準備する。既定はno-op。PreparationContextは保存された呼出し主体・Run/claim・安定key・attempt・targetを渡す。proofは不透明bytes（1..65536）、世代識別子（1..256 bytes）、Clockと同単位の絶対期限を持つ。準備失敗・proof欠落・不正/期限切れはintent保存・予約・送信より前に失敗する。SDK固有scopeや認証形式は公開契約に含めない。

`DispatchRequest.recovery_proof`は最終要求・intent・全祖先予約と同じbegin境界に固定する。invoke/lookup/cancelのToolContextから元証明を参照できる。既存操作では準備を再実行せず、保存済み要求と証明で照合する。adapterが現在の保存世代・lease・ownerとの一致を検証する。新attemptの証明取得でも元予約snapshotのrecovery_untilを延長しない。準備済み操作のNotFoundはproof.generationと照合し、Toolのdescriptor digestであるscopeとは分ける。初回準備失敗で予約を作らないことと、既存Unknownの予約を保持することは別契約である。RegisteredToolとStoredOutputToolも準備宣言・hookを転送する。

`ExternalResult::{Applied,Failed}.physical_evidence`は接続先が報告した物理送信の観測列。各観測は順序番号と不透明bytes、列全体はComplete/Truncatedを持ち、最大256観測・合計65536 bytes、順序番号は狭義増加とする。切詰めはadapterが宣言し、Runtrellは黙って切り捨てず上限違反を拒否する。これは`Operation.usage_observations`（Runtrellの論理結果の使用量履歴）とは別物。結果と一緒にMemory/Pgへ保存し、再照合・同値結果の重複精算防止に使う。証拠bytesをRuntrellが料金へ解釈することはなく、Usageの測定・完全性検査も引き続き必要となる。

公開async traitはSend futureを返す静的dispatch契約。RegisteredToolのerased futureもSendであり、所有したRuntime/claimをmulti-thread Tokioのtokio::spawnへ渡せる。ホストはsupervisor・blocking taskの完了までTokio runtimeを維持する。blocking taskのFuture取消は物理I/Oの取消・保存失敗を意味しない。

最終候補のJSON構文エラーは`JSON_SYNTAX` diagnosticとして既存の自律修正経路へ返す。`schema::parse_json`は原因・hint、1起算の行/byte列、0起算のbyte offset、半開区間spanを返す（EOFのspanは末尾の空区間）。不正JSONには信頼できるinstance treeがないためpathはroot pointerとし、近傍の項目を推測しない。元の候補はJSON文字列Valueとしてreceiptのhashへ結び付け、protocol・候補・診断・修正回数・開始時刻をcheckpointへ保存する。構文エラーのときはSchema/business validatorとfinalizerを実行しない。正常なJSON文字列は通常のValueとして検証する。

JSON値の`digest`はobjectのキーを再帰的に整列して計算し、Postgres JSONB再読取りのキー順変更でもreceiptを同じ候補へ照合する。array順と値の違いは保持する。Artifactのraw bytes hashは従来どおりbytesそのものに結び付ける。

修正可否のcodeと回数・時間はhostのValidator/RepairPolicyが供給する。既定ValidatorはJSON_SYNTAXとSCHEMA_VALIDATIONを修正可能とし、ValidationChainはhostがrepairable_codesへ明示する。追加モデル呼出しは別operationとして通常の予約・精算を行い、元操作の結果・精算は再利用する。修正不能/上限到達ではlast_validationへ候補と診断を保持して理由付き終了、待機中の期限/予算到達でもrepair checkpointを保持する。

成果物bytesはhostが保存adapterで取得し、TargetValidator等から`schema::json_artifact_diagnostics`を利用する。同じ構文診断にArtifact IDを付け、参照のhash/bytesと不一致ならConflictで拒否する。対象選択・保存先・検証基準はhostの責任、構文判定と修正機構は共通基盤。試験の成果物保存はMemory DataStoreで、Postgresは制御checkpoint/操作台帳を担う。

## 選択可能な標準ツール（rt-5）

正本は[standard_tools.rs](../../crates/runtrell/src/standard_tools.rs)と[execution/tool.rs](../../crates/runtrell/src/execution/tool.rs)。`ToolSet<S>`は空から開始し、`files(store, access)`・`validation(tool)`・`execution(tool)`で資源を接続する。接続だけでは有効化しない。`select(StandardTool)`で選び、`disable`で除外する。`ToolPreset(BTreeMap<StandardTool, ToolSettings>)`は集合と設定の既定値。後続の`settings`はそのツールの設定を置換する。timeout・出力上限・予約額・StoredOutputのinline上限を個別に指定できる。Presetと設定型はserde対応で、configuration_digestへ渡す構成本文に含められる。拡張は別途`RegisteredTool::new(custom_tool)`で追加する。通常の標準機能利用に独自Toolの実装は不要。

`StandardTool::requirements`が接続・資源を宣言する。`build`は選択済みツールについてのみ不足を検査し、`ConfigDiagnostic {code, location, message, fix}`を全件返す。実行backend未接続は`EXECUTION_BACKEND_MISSING`、他の必須資源不足は`TOOL_RESOURCE_MISSING`、必要制限非対応は`EXECUTION_CAPABILITY_MISSING`。未選択の能力を補完・起動しない。storeは`StandardToolStore`の共通接続型を使うため、読取りだけの場合も同型のArtifact契約を実装するが、未選択のArtifact操作は提示しない。

三つのゲートを分ける。(1) Cargo featureはコンパイルする能力。現行の標準Rust adapterは常に利用可能で、Python builder/script variantだけ`python`で追加する。(2) ToolSetの選択結果と`pin_catalog(run, policy, tools)`がAgentへ提示する定義。(3) 現在の`Policy::authorize`が個々のdispatch・開始・再開・保存を認可する。featureや選択は認可の代わりにならない。

`Tool::operation_kind`は通常Tool、ExecutionToolはSandbox。RegisteredTool・設定adapter・StoredOutputToolはこれを保持する。driverはSandbox toolでも同じcatalog digest・account・version・operation kindを照合して取消・復旧へ配送する。低水準`execute_backend`は引き続きhostが明示的に実行する共通契約であり、モデル向けtoolの互換wrapperではない。hostはExecutionToolとRuntimeへ同じControlStore・Policy・Clockを接続する。

<!-- generated-contracts -->
## Rust契約定義

`cargo run -p xtask -- api-doc`で公開ソースから生成。型の全保存フィールド、Result/Errorとenumは[api.rs](../../crates/runtrell/src/api.rs)を参照する。以下は同じ正本の抜粋であり独立した仕様ではない。

```rust
pub struct RunSpec {
    pub tenant: String,
    pub principal: String,
    pub request_key: String,
    pub account: String,
    pub parent: Option<RunHandle>,
    pub session: Option<String>,
    pub context_revision: u64,
    pub basis_revision: u64,
    pub config_version: String,
    pub input: String,
    pub purpose: String,
    pub deadline: Option<u64>,
    pub trigger: Trigger,
    pub validator: String,
    pub validator_version: String,
    pub budget: ReservationPolicy,
    pub scheduling: SchedulingPolicy,
}

pub struct RunHandle {
    pub tenant: String,
    pub id: String,
}

pub struct ToolContext {
    pub recovery_proof: Option<RecoveryProof>,
    pub claim: Claim,
    pub principal: String,
    pub operation: String,
    pub attempt: u32,
    pub deadline: Option<u64>,
    pub permission: String,
    pub account_binding: String,
}

pub struct RecoveryProof {
    pub bytes: Vec<u8>,
    pub generation: String,
    /// Absolute Clock timestamp. A later preparation must never extend an old reservation.
    pub expires_at: u64,
}

pub struct PreparationContext {
    pub claim: Claim,
    pub principal: String,
    pub key: String,
    pub attempt: u32,
    pub target: String,
}

pub struct PhysicalSendObservation {
    pub sequence: u32,
    pub bytes: Vec<u8>,
}

pub struct PhysicalSendEvidence {
    pub observations: Vec<PhysicalSendObservation>,
    pub completeness: EvidenceCompleteness,
}

pub struct ToolDescriptor {
    pub name: String,
    pub version: String,
    pub account: String,
    pub permission: String,
    pub capability: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub side_effect: bool,
    pub timeout: u64,
    pub max_output_bytes: usize,
    pub reserve: Amount,
    pub quantities: BTreeMap<String, u64>,
    pub conflict_scope: Option<String>,
    pub retry: RetryPolicy,
}

pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDescriptor>,
    pub max_output: u64,
    pub metadata: Value,
}

pub struct ModelReply {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub complete: bool,
    pub metadata: Value,
}

pub struct ValidationReceipt {
    pub hash: String,
    pub basis_revision: u64,
    pub config_version: String,
    pub validator: String,
    pub validator_version: String,
    pub passed: bool,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct Diagnostic {
    pub code: String,
    pub severity: String,
    /// Instance JSON Pointer (or business diagnostic location).
    pub path: String,
    /// Schema JSON Pointer for schema diagnostics.
    pub schema_path: Option<String>,
    pub artifact: Option<String>,
    pub file: Option<String>,
    pub field: Option<String>,
    /// Half-open byte span in the diagnostic target.
    pub span: Option<(u64, u64)>,
    pub position: Option<JsonPosition>,
    pub cause: String,
    pub hint: String,
}

pub struct JsonPosition {
    pub line: usize,
    pub column: usize,
    pub byte_offset: usize,
}

pub trait Clock: Send + Sync {
    fn now(&self) -> u64;
    fn wait_until(&self, deadline: u64) -> impl std::future::Future<Output = ()> + Send;
}
pub trait Policy: Send + Sync {
    fn authorize(&self, run: &RunRecord, target: &str, permission: &str) -> Result<()>;
}
pub trait ModelPort: Send + Sync {
    /// Only declared adapters are prepared; ordinary local tools need no proof.
    fn requires_recovery_preparation(&self) -> bool {
        false
    }
    fn prepare_recovery(
        &self,
        _context: &PreparationContext,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Option<RecoveryProof>>> + Send {
        async move { Ok(None) }
    }

    fn cancel(
        &self,
        _context: &ToolContext,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send {
        async move { Ok(Progress::Unknown) }
    }
    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 3,
            backoff: 1,
            max_elapsed: 60,
        }
    }
    fn limits(&self) -> ModelLimits;
    fn input_upper_bound(&self, request: &ModelRequest) -> Result<u64>;
    fn recovery(&self) -> Option<(String, u64)> {
        None
    }
    fn invoke(
        &self,
        context: &ToolContext,
        request: ModelRequest,
    ) -> impl std::future::Future<Output = Result<ExternalResult>> + Send;
    fn lookup(
        &self,
        context: &ToolContext,
        request: ModelRequest,
    ) -> impl std::future::Future<Output = Result<Lookup>> + Send;
}
pub trait Tool: Send + Sync {
    /// Resource admission class; execution tools reserve Sandbox slots.
    fn operation_kind(&self) -> OperationKind {
        OperationKind::Tool
    }
    /// Only declared adapters are prepared; ordinary local tools need no proof.
    fn requires_recovery_preparation(&self) -> bool {
        false
    }
    fn prepare_recovery(
        &self,
        _context: &PreparationContext,
        _input: &Value,
    ) -> impl std::future::Future<Output = Result<Option<RecoveryProof>>> + Send {
        async move { Ok(None) }
    }

    fn cancel(
        &self,
        _context: &ToolContext,
        _input: Value,
    ) -> impl std::future::Future<Output = Result<Progress>> + Send {
        async move { Ok(Progress::Unknown) }
    }
    fn descriptor(&self) -> ToolDescriptor;
    fn invoke(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> impl std::future::Future<Output = Result<ExternalResult>> + Send;
    fn lookup(
        &self,
        context: &ToolContext,
        input: Value,
    ) -> impl std::future::Future<Output = Result<Lookup>> + Send;
}
/// Host supplies limits and business-specific diagnostic classification.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RepairPolicy {
    pub max_repairs: Option<u64>,
    pub max_elapsed: Option<u64>,
}
pub trait Validator: Send + Sync {
    fn repair_policy(&self) -> RepairPolicy {
        RepairPolicy::default()
    }
    fn repairable(&self, diagnostics: &[Diagnostic]) -> bool {
        !diagnostics.is_empty()
            && diagnostics.iter().all(|d| {
                d.severity == "error"
                    && matches!(d.code.as_str(), "SCHEMA_VALIDATION" | "JSON_SYNTAX")
            })
    }
    fn validate(&self, run: &RunRecord, candidate: &Value) -> Result<ValidationReceipt>;
}
pub trait Finalizer: Send + Sync {
    fn finalize(
        &self,
        run: &RunRecord,
        candidate: Value,
        receipt: ValidationReceipt,
    ) -> Result<Completion>;
}
pub trait SecretProvider: Send + Sync {
    fn with_secret<R: Send>(
        &self,
        context: &ToolContext,
        target: &str,
        use_secret: impl FnOnce(&[u8]) -> Result<R> + Send,
    ) -> impl std::future::Future<Output = Result<R>> + Send;
}
pub trait Observer {
    fn observe(&self, event: &Event);
}
#[derive(Clone, Debug)]
pub struct Event {
    pub run: RunHandle,
    pub operation: Option<String>,
    pub revision: u64,
    pub code: String,
}

pub trait RunStore: Send + Sync {
    fn renew(
        &self,
        claim: &Claim,
        now: u64,
        lease: u64,
    ) -> impl std::future::Future<Output = Result<Claim>> + Send;
    fn find_request(
        &self,
        spec: &RunSpec,
    ) -> impl std::future::Future<Output = Result<Option<RunHandle>>> + Send;
    fn start(&self, spec: RunSpec) -> impl std::future::Future<Output = Result<RunHandle>> + Send;
    fn get(&self, run: &RunHandle) -> impl std::future::Future<Output = Result<RunRecord>> + Send;
    fn claim(
        &self,
        run: &RunHandle,
        worker: &str,
        now: u64,
        lease: u64,
    ) -> impl std::future::Future<Output = Result<Claim>> + Send;
    fn check(
        &self,
        claim: &Claim,
        now: u64,
        new_work: bool,
    ) -> impl std::future::Future<Output = Result<RunRecord>> + Send;
    fn checkpoint(
        &self,
        claim: &Claim,
        expected: u64,
        checkpoint: Checkpoint,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn cancel(
        &self,
        run: &RunHandle,
        reason: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn terminate(
        &self,
        claim: &Claim,
        terminal: Terminal,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn finalize(
        &self,
        claim: &Claim,
        completion: Completion,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn wait(
        &self,
        claim: &Claim,
        reason: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn cleanup(
        &self,
        claim: &Claim,
        stop: Progress,
        cleanup: Progress,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
pub trait BudgetAuthority: Send + Sync {
    fn create_account_with_policy(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
        policy: AccountPolicy,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    /// Fenced, zero-charge finalization admission; does not authorize new I/O.
    fn finalization_allowed(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn exhaust(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn create_account(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    fn account(&self, id: &str) -> impl std::future::Future<Output = Result<Account>> + Send;
    fn remaining(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Amount>> + Send;
    fn warnings(
        &self,
        claim: &Claim,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Vec<Warning>>> + Send;
}
pub trait OperationStore: Send + Sync {
    fn observe_result(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    fn proven_unsent(
        &self,
        claim: &Claim,
        id: &str,
        scope: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    /// Atomic reservation across all ancestors plus intent; no I/O before success.
    fn begin(
        &self,
        claim: &Claim,
        request: DispatchRequest,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Operation>> + Send;
    fn list_operations(
        &self,
        run: &RunHandle,
    ) -> impl std::future::Future<Output = Result<Vec<Operation>>> + Send;
    fn operation(
        &self,
        run: &RunHandle,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<Operation>>> + Send;
    /// Every send, including a proven-not-sent replay, requires a fresh fence.
    fn mark_sent(
        &self,
        claim: &Claim,
        id: &str,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
    /// Atomic result and settlement. Existing operations remain writable after cancellation.
    fn record(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotRequest {
    pub key: String,
    pub tenant: String,
    pub run: String,
    pub kind: OperationKind,
}
#[derive(Clone, Debug)]
pub struct QueueEntry {
    pub key: String,
    pub tenant: String,
    pub scheduling: SchedulingPolicy,
    pub deadline: u64,
}
#[derive(Clone, Debug)]
pub struct Capacity {
    pub global: BTreeMap<OperationKind, usize>,
    pub tenant: BTreeMap<OperationKind, usize>,
    pub run: BTreeMap<OperationKind, usize>,
    pub queue: usize,
    pub tenant_queue: usize,
}
impl Default for Capacity {
    fn default() -> Self {
        let limits = BTreeMap::from([
            (OperationKind::Run, 16),
            (OperationKind::Model, 8),
            (OperationKind::Tool, 8),
            (OperationKind::Sandbox, 2),
            (OperationKind::ChildRun, 4),
            (OperationKind::Retry, 8),
            (OperationKind::Compaction, 2),
        ]);
        Self {
            global: limits.clone(),
            tenant: limits.clone(),
            run: limits,
            queue: 64,
            tenant_queue: 16,
        }
    }
}
pub trait Scheduler: Send + Sync {
    fn acquire(&self, request: SlotRequest)
    -> impl std::future::Future<Output = Result<()>> + Send;
    fn release_slot(&self, key: &str) -> impl std::future::Future<Output = Result<()>> + Send;
    fn enqueue(&self, entry: QueueEntry) -> impl std::future::Future<Output = Result<()>> + Send;
    fn dequeue(
        &self,
        now: u64,
    ) -> impl std::future::Future<Output = Result<Option<QueueEntry>>> + Send;
}
pub trait ControlStore: RunStore + BudgetAuthority + OperationStore + Scheduler {}
impl<T: RunStore + BudgetAuthority + OperationStore + Scheduler> ControlStore for T {}

```
