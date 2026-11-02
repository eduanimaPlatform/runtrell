# Budget mechanism and host policy

RunSpec fixes a host-supplied RunBudgetPolicy: reservation denial ends the account
(default), ends only this Run, waits until a finite absolute deadline, or shrinks
model output down to a configured minimum (failure to reach that minimum ends
only this Run). Wait deadlines use the host Clock epoch and are saved in RunSpec;
retrying a waiting Run does not extend them. ModelLimits.min_max_output_tokens is
the provider/model's technical minimum accepted max-output request parameter.
Shrink.min_output_budget_tokens is the host's quality floor for the reserved
output allowance. Shrinking respects both floors; neither guarantees a minimum
number of actually generated tokens. Non-model requests cannot be shrunk. A failed
reservation creates no intent and sends no I/O.

AccountPolicy fixes enabled token/cost axes (both by default), optional named
host quantity limits, warning percentages (50/80/95 by default), whether a fully
reserved unknown result permits other work (false), and whether zero-charge
finalization after charging stops is permitted (false). Ancestor policies all
apply. Disabled axes still retain measured usage. Policy is immutable after
account creation. Unknown reservations and slots remain held until settlement;
continuation never means release. Finalization still requires settled operations,
current owner, current revisions, and a trusted receipt.

Usage distinguishes missing measurements from measured zero. Image input is a
subset of input; cached image input is the intersection of cached and image
input, with max(0,cached+image-input) <= intersection <= min(cached,image).
Image output and reasoning are each a subset of output (each <= output); their
sum may exceed output because providers do not promise they are disjoint and do
not report the overlap. A category billed at the plain output rate cannot change
the cost and is folded into ordinary output; when both carry their own rate and
both are nonzero, the overlap decides the price, so settlement is refused as
unknown and the reservation is kept (tests `reasoning_and_image_output_*`). Price snapshots use
integer micro-USD per million tokens. The billing unit is one billed attempt
(one settlement of one operation attempt). Its cost is
`ceil( sum_k(tokens_k * rate_k) / 1_000_000 )` over the disjoint categories
(ordinary input, cached-only, image-only, cached image, ordinary output,
reasoning, image output), accumulated in u128 and rounded up exactly once for
the attempt, then checked into u64. Categories are never rounded separately
(test `categories_are_summed_before_one_round_up_per_billed_attempt`). Missing
subsets are only dispensable when their rates are equal. Unknown price or
required usage prevents settlement and preserves the reservation. Fixed-price
jobs use measured zero tokens and an explicit cost; measured zero cost identifies
free operations. Host quantity observations settle in the same transaction. ToolDescriptor.quantities
provides the named quantity upper bounds pinned in the catalog and forwarded to
the dispatch reservation by the driver.

Usage.total validates measured subsets and returns input + output without requiring
a price or measured cost. Subsets are never added again to the total. Price.estimate
quotes an input upper bound plus an output allowance at the highest applicable
input/output category rates, with the same single ceiling and overflow checks as
actual billing. ModelLimits.estimate also validates the model/price identity and
technical request bounds (including context). ModelLimits.cap selects the largest
affordable allowance using that same quote. No built-in model prices or provider
formats are part of this API; hosts supply snapshots and normalized measurements.

An operation key identifies logical work; attempt identifies a billed attempt.
Lookup/replay of one result settles that attempt once. Separately charged retry
attempts each add their usage; provider adapters must expose them individually.

A `max_tokens` response is charged before continuation. Its partial tool calls
are discarded, and a new operation quotes input, context headroom, output limit
and the remaining account together. The host may set a continuation count limit;
there is no default count ceiling. Budget policy and deadline still apply.
Automatic context summaries use Compaction operations, the host-selected summary
model's price and limits, and the same account. A summary is committed with CAS
only after complete measured success; unknown usage keeps the reservation.
Schema and host-repairable failures use fresh reserved model turns, with optional
host count/time limits shared across final-output and tool-input repairs.

SchedulingPolicy defaults to priority=0 and weight=2. Priority is an integer;
weight must be positive. Tenant rotation precedes priority, and the scheduler saves
a shared priority streak, preserving the existing bounded priority burst. Equal priorities are FIFO; after the configured
burst the oldest lower priority entry is selected. No business purpose string is
interpreted by the scheduler.

Every unknown retains its full reservation. An Accepted operation with an
explicit zero upper bound and no reserved quantities need not block the account,
but still holds its slot and prevents Run finalization until its receipt is
confirmed. Missing measurements on a completed result return UsageUnknown after
the incomplete result and usage observation are saved; malformed subsets return
Invalid. Memory and Postgres implement this same contract.


The driver computes the final model max_output and reservation once as one quote.
A competing Run cannot replace just the reserved amount with a smaller quote.
If atomic admission loses a race under Shrink, the driver returns Waiting("budget
requote") without sending, releases its active Run slot, and recomputes both values
on resume. No operation intent was admitted. Other denial policies retain their
ordinary refusal/wait behavior. Input estimation is performed before this final
quote and must bound the finalized request. The shared Memory/Postgres
`model_quote_race` contract inserts a reservation by another Run between quoting
and admission, then checks the resumed request and reservation together.

Persisted complete results are recovered through the original attempt before a
provider lookup. The store fences observations at write time and checks the
current owner again at settlement. Conflicting complete observations are refused.
Incomplete observations retain reservations; late results from stale owners
require independent host verification and are never automatically adopted. After
settlement, result is authoritative and temporary observation copies are cleared.

## Price lines and owner settlement recovery

`Price` rates are integer micro-USD per million tokens and the input side is a partition of four
disjoint lines, so a token is billed once and every rate is independent: `input` (neither cached
nor image), `cached` (cached, not image), `image_input` (image, not cached) and
`cached_image_input` (both). With `Usage` totals (`cached` and `image_input` include their
intersection `cached_image_input`) the lines are `i - cached - image + both`, `cached - both`,
`image - both` and `both`; the sum is rounded up once. A counter that can change the bill and is
`None` makes `Price::cost` return `UsageUnknown`; a category with equal rates needs no count.

A provider result that is present and complete but whose usage never settled keeps its reservation
and unknown mark (`Operation::awaits_operator`). Asking again can only return the same evidence,
so the Run enters `Waiting(OPERATOR_ACTION_WAIT)` once the operation has no lookup window left
(`Operation::stalled`: `now >= recovery_until` in Unix milliseconds, or never had one) and the
same evidence was just confirmed: a Run that is not yet parked (new worker, or another wait) looks
the unsettled operations up once and parks only if nothing changed; a parked Run asks nothing and
sends nothing until the owner changes it.

A result that is **missing or incomplete** is not this case. A model reply flagged
`complete: false`, or a model result with no input/output totals (for example a response lost in
transit and saved as an empty reply), keeps the ordinary lookup/recovery path: the reopened Run
looks it up, recovers the reply or ends `OutcomeUnknown`, never resends and holds the
reservation. Tool results are the saved receipt itself, so a saved tool result is complete. While a lookup window is
open the Run keeps its ordinary `external reconciliation` wait, because the adapter may still
supply corrected measurements. There is no retry count: the state follows the evidence.

`OperationStore::settle_recovered(run, operation, SettlementRecovery, now)` is the owner path
out. The request carries a fixed `basis` string, the stored usage refined by a documented rule
and the price basis. The store accepts it only if the operation is Applied/Failed, unsettled and
unknown, the new usage only fills counters that were `None` (model and quantities equal,
`micro_usd` still `None`), `price.model` equals the usage and pinned price model, and
`Price::cost` can compute the bill; otherwise nothing changes (`UsageUnknown` stays for the
operator). It then settles the original operation in one transaction: releases the reservation
and unknown mark, charges the corrected amount to every ancestor account, replaces the saved
result's usage, and stores `Operation.recovery` with the old usage, price basis, new usage,
amount and Unix-millisecond time. It makes no provider call and takes no Run claim, so callers
authorize the operator first. An identical repeat returns the stored record without charging
again; a different basis or price returns `Conflict`. `runtrell-providers` supplies the
documented rule for OpenAI Images (`recovery_for_stored_openai_images_usage`).
