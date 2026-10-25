# runtrell API（2a control schema 1 / data schema 2）

依存は host → `runtrell` → `runtrell-protocol`。fake host → `runtrell-testkit` → `runtrell`。新規コードはApache-2.0。crate名は実行機構、判断protocol、試験接続の責務を示す作業名で、registry名の取得を意味しない。protocolの型はruntime APIで再exportしない。

公開Rust型・traitの正本は `crates/runtrell/src/api.rs`。RunSpecはtenant、principal、重複request key、入力/目的、共有account、親、任意sessionと期待context版、根拠版、設定版、任意deadlineを固定する。RunHandleはtenant付きID。ToolContextは信頼済みdriverがclaimと固定descriptorから作る。model/toolが操作IDや権限を選び直さない。

RunStoreはstart/get/claim/checkpoint/cancel/terminate/finalize、OperationStoreはbegin/operation/mark_sent/record、BudgetAuthorityはcreate_account/accountを提供する。ControlStoreはこれらと共有slot・queue・警告配信を組み合わせる。beginは現在owner・取消・期限・accountを再検査し、全祖先accountの予約とintentを同じbarrierで保存する。recordは操作結果と精算を同じbarrierで保存する。分離adapterはこの原子性を実装せずにControlStoreを名乗れない。

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

`FileTool<S>`/`StoredOutputTool<T,S>`は`StandardToolStore`（WorkspaceStore + ArtifactStore）を、`DataValidator<V,S>`/`DataFinalizer<S>`は`EvidenceStore`を要求する。context圧縮は`ContextStore`を受け取る。既定型はDataStoreであり、外部adapterはtraitを直接実装するか、`NamespaceBackend`のopaque state読取/CASへ接続して共通アルゴリズムを再利用する。同期Validator/Finalizerに合わせた同期保存境界であり、hostは必要ならblocking executorへ配置する。任意の非同期DB clientを直接同期trait内でblock_onすることを推奨するものではない。

`schema::compile_schema`/`schema_diagnostics`/`validate_schema`は同一評価器。SchemaValidatorはTargetValidatorとして保存証跡へ接続でき、ValidationChainは構造化出力に使える。未知keywordを検査する前処理は対応範囲の拒否専用で、インスタンスの検証はjsonschemaへ委譲する。Schema構成エラーとインスタンス不合格は区別し、診断はinstance pointer（path）・schema pointer（schema_path）・原因を保持する。

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
    pub claim: Claim,
    pub principal: String,
    pub operation: String,
    pub attempt: u32,
    pub deadline: Option<u64>,
    pub permission: String,
    pub account_binding: String,
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

pub trait Clock: Send + Sync {
    fn now(&self) -> u64;
    async fn wait_until(&self, deadline: u64);
}
pub trait Policy: Send + Sync {
    fn authorize(&self, run: &RunRecord, target: &str, permission: &str) -> Result<()>;
}
pub trait ModelPort: Send + Sync {
    async fn cancel(&self, _context: &ToolContext) -> Result<Progress> {
        Ok(Progress::Unknown)
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
    fn recovery(&self) -> Option<(String, u64)>;
    async fn invoke(&self, context: &ToolContext, request: ModelRequest) -> Result<ExternalResult>;
    async fn lookup(&self, context: &ToolContext, request: ModelRequest) -> Result<Lookup>;
}
pub trait Tool: Send + Sync {
    async fn cancel(&self, _context: &ToolContext, _input: Value) -> Result<Progress> {
        Ok(Progress::Unknown)
    }
    fn descriptor(&self) -> ToolDescriptor;
    async fn invoke(&self, context: &ToolContext, input: Value) -> Result<ExternalResult>;
    async fn lookup(&self, context: &ToolContext, input: Value) -> Result<Lookup>;
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
            && diagnostics
                .iter()
                .all(|d| d.severity == "error" && d.code == "SCHEMA_VALIDATION")
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
pub trait SecretProvider {
    async fn with_secret<R>(
        &self,
        context: &ToolContext,
        target: &str,
        use_secret: impl FnOnce(&[u8]) -> Result<R> + Send,
    ) -> Result<R>;
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
    async fn renew(&self, claim: &Claim, now: u64, lease: u64) -> Result<Claim>;
    async fn find_request(&self, spec: &RunSpec) -> Result<Option<RunHandle>>;
    async fn start(&self, spec: RunSpec) -> Result<RunHandle>;
    async fn get(&self, run: &RunHandle) -> Result<RunRecord>;
    async fn claim(&self, run: &RunHandle, worker: &str, now: u64, lease: u64) -> Result<Claim>;
    async fn check(&self, claim: &Claim, now: u64, new_work: bool) -> Result<RunRecord>;
    async fn checkpoint(
        &self,
        claim: &Claim,
        expected: u64,
        checkpoint: Checkpoint,
        now: u64,
    ) -> Result<()>;
    async fn cancel(&self, run: &RunHandle, reason: &str) -> Result<()>;
    async fn terminate(&self, claim: &Claim, terminal: Terminal, now: u64) -> Result<()>;
    async fn finalize(&self, claim: &Claim, completion: Completion, now: u64) -> Result<()>;
    async fn wait(&self, claim: &Claim, reason: &str, now: u64) -> Result<()>;
    async fn cleanup(
        &self,
        claim: &Claim,
        stop: Progress,
        cleanup: Progress,
        now: u64,
    ) -> Result<()>;
}
pub trait BudgetAuthority: Send + Sync {
    async fn create_account_with_policy(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
        policy: AccountPolicy,
    ) -> Result<String>;
    /// Fenced, zero-charge finalization admission; does not authorize new I/O.
    async fn finalization_allowed(&self, claim: &Claim, now: u64) -> Result<()>;
    async fn exhaust(&self, claim: &Claim, now: u64) -> Result<()>;
    async fn create_account(
        &self,
        tenant: &str,
        limit: Amount,
        parent: Option<String>,
    ) -> Result<String>;
    async fn account(&self, id: &str) -> Result<Account>;
    async fn remaining(&self, claim: &Claim, now: u64) -> Result<Amount>;
    async fn warnings(&self, claim: &Claim, now: u64) -> Result<Vec<Warning>>;
}
pub trait OperationStore: Send + Sync {
    async fn observe_result(
        &self,
        claim: &Claim,
        id: &str,
        result: ExternalResult,
        now: u64,
    ) -> Result<()>;
    async fn proven_unsent(&self, claim: &Claim, id: &str, scope: &str, now: u64) -> Result<()>;
    /// Atomic reservation across all ancestors plus intent; no I/O before success.
    async fn begin(&self, claim: &Claim, request: DispatchRequest, now: u64) -> Result<Operation>;
    async fn list_operations(&self, run: &RunHandle) -> Result<Vec<Operation>>;
    async fn operation(&self, run: &RunHandle, key: &str) -> Result<Option<Operation>>;
    /// Every send, including a proven-not-sent replay, requires a fresh fence.
    async fn mark_sent(&self, claim: &Claim, id: &str, now: u64) -> Result<()>;
    /// Atomic result and settlement. Existing operations remain writable after cancellation.
    async fn record(&self, claim: &Claim, id: &str, result: ExternalResult, now: u64)
    -> Result<()>;
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
    async fn acquire(&self, request: SlotRequest) -> Result<()>;
    async fn release_slot(&self, key: &str) -> Result<()>;
    async fn enqueue(&self, entry: QueueEntry) -> Result<()>;
    async fn dequeue(&self, now: u64) -> Result<Option<QueueEntry>>;
}
pub trait ControlStore: RunStore + BudgetAuthority + OperationStore + Scheduler {}
impl<T: RunStore + BudgetAuthority + OperationStore + Scheduler> ControlStore for T {}

```
