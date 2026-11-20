// SPDX-License-Identifier: Apache-2.0
use super::{pricing::*, *};
use crate::{Amount, Error, Usage};
use serde_json::{Value, json};
use std::collections::BTreeSet;

const EXAMPLE: &str = include_str!("fixtures/pricing-example.json");

fn cell(fields: Value) -> Cell {
    serde_json::from_value(fields).unwrap()
}

fn item(amount: Measured, scope: Scope, bill_on: BillOn) -> Vec<Item> {
    vec![Item {
        amount,
        scope,
        group: None,
        eligible: BTreeSet::from([bill_on]),
    }]
}

struct QuoteExample<'a> {
    catalog: &'a Catalog,
    cells: Map<Cell>,
    measurements: Map<Map<Vec<Item>>>,
    remaining: Map<num_rational::BigRational>,
}

impl QuoteExample<'_> {
    fn bill(&self, roots: &[&str], reserving: bool) -> crate::Result<u64> {
        Quote {
            profiles: &self.catalog.price_profiles,
            cells: &self.cells,
            time_windows: &self.catalog.time_windows,
            measurements: &self.measurements,
            remaining: self.remaining.clone(),
            activated: true,
            reserving,
        }
        .usd(&roots.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }
}

#[test]
fn full_request_band_applies_independent_multipliers_before_single_rounding() {
    let catalog: Catalog = serde_json::from_str(EXAMPLE).unwrap();
    let fields = cell(json!({"prompt_tokens":100001,"tier":"batch","residency":"us_only"}));
    let quote = QuoteExample {
        catalog: &catalog,
        cells: Map::from([
            ("haiku-tokens".into(), fields),
            ("claude-web-search".into(), Cell::new()),
        ]),
        measurements: Map::from([
            (
                "haiku-tokens".into(),
                Map::from([
                    ("input".into(), vec![Item::measured(20_001)]),
                    ("read".into(), vec![Item::measured(20_000)]),
                    ("write-5m".into(), vec![Item::measured(30_000)]),
                    ("write-1h".into(), vec![Item::measured(30_000)]),
                    ("output".into(), vec![Item::measured(2000)]),
                ]),
            ),
            (
                "claude-web-search".into(),
                Map::from([(
                    "searches".into(),
                    item(Measured::Units(2), Scope::Attempt, BillOn::Success),
                )]),
            ),
        ]),
        remaining: Map::new(),
    };
    let roots = ["haiku-tokens", "claude-web-search"];
    assert_eq!(quote.bill(&roots, false), Ok(55_613));
}

#[test]
fn duration_quantization_and_shared_quantity_allowance() {
    let catalog: Catalog = serde_json::from_str(EXAMPLE).unwrap();
    let id = "deepseek-asr-duration".to_string();
    let query = "gemini-search-queries".to_string();
    let mut quote = QuoteExample {
        catalog: &catalog,
        cells: Map::from([
            (id.clone(), cell(json!({"promotion":false}))),
            (query.clone(), Cell::new()),
        ]),
        measurements: Map::from([
            (
                id,
                Map::from([(
                    "audio-in".into(),
                    item(Measured::Units(3200), Scope::Item, BillOn::Measured),
                )]),
            ),
            (
                query,
                Map::from([(
                    "queries".into(),
                    item(Measured::Units(3), Scope::Attempt, BillOn::Measured),
                )]),
            ),
        ]),
        remaining: Map::new(),
    };
    assert_eq!(quote.bill(&["deepseek-asr-duration"], false), Ok(129));
    assert_eq!(quote.bill(&["gemini-search-queries"], true), Ok(42_000));
    assert_eq!(
        quote.bill(&["gemini-search-queries"], false),
        Err(Error::UsageUnknown)
    );
    quote
        .remaining
        .insert("gemini-web-search-shared".into(), decimal("1").unwrap());
    assert_eq!(quote.bill(&["gemini-search-queries"], false), Ok(28_000));
}

#[test]
fn size_time_and_marginal_block_rounding_are_independent() {
    let mut catalog: Catalog = serde_json::from_str(EXAMPLE).unwrap();
    let cache = catalog
        .price_profiles
        .get_mut("gemini-cache-storage")
        .unwrap();
    let (cache_component, stored) = cache.components.iter_mut().next().unwrap();
    let cache_component = cache_component.clone();
    // Evaluate documented quantity arithmetic after a future adapter proves its scope.
    stored.rounding_known = true;
    let id = "tiered-pixels";
    catalog.price_profiles.insert(id.into(), serde_json::from_value(json!({
        "billing_provider":"openai", "payer":"direct","currency":"USD","purpose":"billable",
        "clock":"accepted_at",
        "components":{"pixel":{
            "meter":{"unit":"pixel","direction":"output","modalities":["image"],"class":"base"},
            "bill_on":"measured","rounding_known":true,
            "quantization":[{"axis":"quantity","scope":"item","round":"ceil","step":"1000000","minimum":"0"}],
            "rates":[{"state":"priced","per":"1000000","marginal":[
                {"quantity":{"min":"0","max_exclusive":"1000000"},"amount":"0.03"},
                {"quantity":{"min":"1000000"},"amount":"0.015"}
            ]}]
        }}
    })).unwrap());
    let quote = QuoteExample {
        catalog: &catalog,
        cells: Map::from([
            (id.into(), Cell::new()),
            ("gemini-cache-storage".into(), Cell::new()),
        ]),
        measurements: Map::from([
            (
                "gemini-cache-storage".into(),
                Map::from([(
                    cache_component,
                    item(
                        Measured::SizeTime {
                            size: 100_000,
                            ms: 1_800_000,
                        },
                        Scope::Resource,
                        BillOn::Measured,
                    ),
                )]),
            ),
            (
                id.into(),
                Map::from([(
                    "pixel".into(),
                    item(Measured::Units(1_100_000), Scope::Item, BillOn::Measured),
                )]),
            ),
        ]),
        remaining: Map::new(),
    };
    assert_eq!(quote.bill(&["gemini-cache-storage"], false), Ok(50_000));
    assert_eq!(quote.bill(&[id], false), Ok(45_000));
}

#[test]
fn verified_token_snapshot_keeps_exact_ttl_and_price_revision() {
    let model = resolve_binding(
        "openai/gpt-6.2-sol",
        OperationKind::Completion,
        Some("standard-cache-12h"),
    )
    .unwrap();
    let original = model.pricing();
    let persisted = serde_json::to_value(&original).unwrap();
    let mut changed = persisted.clone();
    changed["catalog_revision"] = json!("future");
    changed["price"]["components"]["input"]["rates"][0]["amount"] = json!("4");
    let newer: PricingSnapshot = serde_json::from_value(changed).unwrap();
    let restored: PricingSnapshot = serde_json::from_value(persisted).unwrap();
    let mut usage = Usage {
        input: Some(100),
        output: Some(50),
        cached: Some(20),
        model: "gpt-6.2-sol".into(),
        quantities: Map::from([(CACHE_WRITE_12H.into(), 10), (CACHE_WRITE_30M.into(), 0)]),
        ..Default::default()
    };
    assert_eq!(restored.cost(&usage), Ok(Amount::new(150, 692)));
    assert_eq!(newer.cost(&usage), Ok(Amount::new(150, 832)));
    usage.quantities.insert(CACHE_WRITE.into(), 11);
    assert_eq!(restored.cost(&usage), Err(Error::UsageUnknown));
}

#[tokio::test]
async fn missing_usage_holds_reservation_and_owner_reuses_the_pinned_snapshot() {
    use crate::{
        BudgetAuthority, DispatchRequest, ExternalResult, OperationStore, OwnerCommand, RunSpec,
        RunStore, SettlementRecovery, memory::MemoryStore,
    };

    let store = MemoryStore::default();
    let account = store
        .create_account("tenant", Amount::new(1000, 1000), None)
        .await
        .unwrap();
    let run = store
        .start(RunSpec::new("tenant", "usage", account.clone(), ""))
        .await
        .unwrap();
    let claim = store.claim(&run, "worker", 1, 1000).await.unwrap();
    let model = resolve("openai/gpt-6.2-sol", OperationKind::Completion).unwrap();
    let snapshot = model.pricing();
    let reserve = snapshot.estimate(100, 50).unwrap();
    let mut dispatch = DispatchRequest::new(
        "managed",
        crate::OperationKind::Model,
        model.wire_model(),
        Value::Null,
        reserve,
    );
    // Simulate a prior release's rate becoming durable before sending.
    let mut prior_price = serde_json::to_value(snapshot).unwrap();
    prior_price["catalog_revision"] = json!("prior");
    prior_price["price"]["components"]["input"]["rates"][0]["amount"] = json!("3");
    dispatch.pricing = Some(serde_json::from_value(prior_price).unwrap());
    let op = store.begin(&claim, dispatch, 2).await.unwrap();
    store.mark_sent(&claim, &op.id, 3).await.unwrap();
    let usage = Usage {
        input: Some(100),
        output: Some(50),
        cached: Some(20),
        model: model.wire_model().into(),
        ..Default::default()
    };
    let response = ExternalResult::Failed {
        code: "FIXTURE_PARTIAL_USAGE".into(),
        usage: usage.clone(),
        repair_hint: None,
        retry_after: None,
        provider_diagnostic: None,
        physical_evidence: None,
    };
    assert_eq!(
        store.record(&claim, &op.id, response, 4).await,
        Err(Error::UsageUnknown)
    );
    let pending = store.account(&account).await.unwrap();
    assert_eq!(pending.reserved, reserve);
    assert_eq!(pending.used, Amount::default());
    assert_eq!(pending.unknown, 1);
    let mut usage = usage;
    usage.quantities.insert(CACHE_WRITE.into(), 0);
    let recovery = SettlementRecovery {
        basis: "fixture-usage@v1".into(),
        usage,
        price: None,
    };
    let command = OwnerCommand {
        key: "settle".into(),
        request_digest: "fixture".into(),
    };
    let receipt = store
        .settle_recovered(&run, &op.id, recovery.clone(), command.clone(), 5)
        .await
        .unwrap();
    assert_eq!(
        store
            .settle_recovered(&run, &op.id, recovery, command, 6)
            .await
            .unwrap(),
        receipt
    );
    let settled = store.account(&account).await.unwrap();
    assert_eq!(settled.reserved, Amount::default());
    assert_eq!(settled.used, Amount::new(150, 742));
    assert_eq!(settled.unknown, 0);
}

#[test]
fn money_allowance_charges_only_the_fee_from_a_reference_subtotal() {
    let mut catalog: Catalog = serde_json::from_str(EXAMPLE).unwrap();
    let mut reference = catalog.price_profiles["router-deepinfra-llama"].clone();
    reference.purpose = "reference".into();
    let rate = &mut reference.components.get_mut("input").unwrap().rates[0];
    rate.amount = Some("10".into());
    rate.per = Some("1000000".into());
    catalog.price_profiles.insert("reference".into(), reference);
    catalog.price_profiles.insert(
        "fee".into(),
        serde_json::from_value(json!({
            "billing_provider":"openrouter","payer":"router","currency":"USD","purpose":"billable",
            "clock":"provider_reported",
            "fees":{"service":{"base_profile":"reference","factor":"0.05","allowance":"money"}}
        }))
        .unwrap(),
    );
    let mut quote = QuoteExample {
        catalog: &catalog,
        cells: Map::from([
            ("fee".into(), Cell::new()),
            ("reference".into(), Cell::new()),
        ]),
        measurements: Map::from([(
            "reference".into(),
            Map::from([
                ("input".into(), vec![Item::measured(1_000_000)]),
                ("output".into(), vec![Item::measured(0)]),
            ]),
        )]),
        remaining: Map::new(),
    };
    assert_eq!(quote.bill(&["fee"], true), Ok(500_000));
    assert_eq!(quote.bill(&["fee"], false), Err(Error::UsageUnknown));
    quote
        .remaining
        .insert("money".into(), decimal("1").unwrap());
    assert_eq!(quote.bill(&["fee"], false), Ok(450_000));
    assert_eq!(
        quote.bill(&["reference"], false),
        Err(Error::PriceNotConfigured)
    );
}

#[test]
fn holiday_coverage_and_time_bands_use_the_billing_clock() {
    let window: validate::TimeWindow = serde_json::from_value(json!({
        "zone":"UTC", "clock":"processed_at", "default_band":"off-peak",
        "weekly":[{"days":["mon","tue","wed","thu","fri"],"start_second":3600,"end_second":14400,"band":"peak"}],
        "calendar_zone":"Asia/Shanghai",
        "calendar_coverage":{"from":"2026-01-01","until":"2027-01-01"},
        "date_overrides":{"2026-01-01":"off-peak"}
    })).unwrap();
    for (date, band) in [
        ("2026-11-20T00:59:59Z", Some("off-peak")),
        ("2026-11-20T01:00:00Z", Some("peak")),
        ("2026-11-20T04:00:00Z", Some("off-peak")),
        ("2026-01-01T01:00:00Z", Some("off-peak")),
        ("2026-12-31T16:00:00Z", None),
    ] {
        assert_eq!(window.band_at(timestamp(date).unwrap()), band);
    }
}
