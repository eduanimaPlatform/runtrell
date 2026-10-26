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
