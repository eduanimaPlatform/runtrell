// SPDX-License-Identifier: Apache-2.0
use super::pricing::*;
use super::*;
use chrono::{Datelike, NaiveDate, Timelike};
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use url::Url;

type Validation<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn require(valid: bool, description: impl fmt::Display) -> Validation {
    if valid {
        Ok(())
    } else {
        Err(description.to_string().into())
    }
}

struct UniqueJson(Value);

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueJson;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueJson, A::Error> {
                let mut value = serde_json::Map::new();
                while let Some((key, UniqueJson(entry))) = map.next_entry::<String, UniqueJson>()? {
                    if value.insert(key.clone(), entry).is_some() {
                        return Err(de::Error::custom(format!("duplicate key {key}")));
                    }
                }
                Ok(UniqueJson(Value::Object(value)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<UniqueJson, A::Error> {
                let mut value = Vec::new();
                while let Some(UniqueJson(item)) = seq.next_element()? {
                    value.push(item);
                }
                Ok(UniqueJson(Value::Array(value)))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<UniqueJson, E> {
                Ok(UniqueJson(json!(value)))
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<UniqueJson, E> {
                Ok(UniqueJson(json!(value)))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<UniqueJson, E> {
                Ok(UniqueJson(json!(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<UniqueJson, E> {
                Ok(UniqueJson(json!(value)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<UniqueJson, E> {
                Ok(UniqueJson(Value::Null))
            }
        }
        d.deserialize_any(UniqueVisitor)
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct TimeWindow {
    pub zone: chrono_tz::Tz,
    pub clock: String,
    pub default_band: String,
    pub weekly: Vec<Weekly>,
    #[serde(default)]
    pub date_overrides: Map<String>,
    pub calendar_coverage: Option<Coverage>,
    pub calendar_zone: Option<chrono_tz::Tz>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Coverage {
    pub from: NaiveDate,
    pub until: NaiveDate,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Weekly {
    pub days: Vec<String>,
    pub start_second: u64,
    pub end_second: u64,
    pub band: String,
}

impl TimeWindow {
    fn bands(&self) -> Vec<Value> {
        let mut bands = BTreeSet::from([&self.default_band]);
        bands.extend(self.weekly.iter().map(|slot| &slot.band));
        bands.extend(self.date_overrides.values());
        bands.into_iter().map(|band| json!(band)).collect()
    }

    pub(crate) fn band_at(&self, milliseconds: i64) -> Option<&str> {
        let utc = chrono::DateTime::from_timestamp_millis(milliseconds)?;
        if let (Some(cal), Some(coverage)) = (self.calendar_zone, &self.calendar_coverage) {
            let date = utc.with_timezone(&cal).date_naive();
            if date < coverage.from || date >= coverage.until {
                return None;
            }
            if let Some(band) = self.date_overrides.get(&date.to_string()) {
                return Some(band);
            }
        }
        let local = utc.with_timezone(&self.zone);
        let day = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"]
            [local.weekday().num_days_from_monday() as usize];
        let time = u64::from(local.num_seconds_from_midnight());
        Some(
            self.weekly
                .iter()
                .find(|slot| {
                    slot.days.iter().any(|d| d == day)
                        && slot.start_second <= time
                        && time < slot.end_second
                })
                .map_or(self.default_band.as_str(), |slot| slot.band.as_str()),
        )
    }

    fn validate(&self, id: &str) -> Validation {
        if let Some(coverage) = &self.calendar_coverage {
            require(
                coverage.from < coverage.until && self.calendar_zone.is_some(),
                id,
            )?;
            for date in self.date_overrides.keys() {
                let date: NaiveDate = date.parse()?;
                require(coverage.from <= date && date < coverage.until, id)?;
            }
        }
        for (index, slot) in self.weekly.iter().enumerate() {
            require(slot.start_second < slot.end_second, id)?;
            for previous in &self.weekly[..index] {
                require(
                    !slot.days.iter().any(|day| previous.days.contains(day))
                        || slot.end_second <= previous.start_second
                        || previous.end_second <= slot.start_second,
                    format_args!("{id}: overlapping weekly time bands"),
                )?;
            }
        }
        Ok(())
    }
}

fn evidence(value: &Value, today: NaiveDate) -> Validation {
    let definitions = value["evidence"].as_object().ok_or("missing evidence")?;
    for (name, entry) in definitions {
        let url = Url::parse(entry["url"].as_str().ok_or("missing URL")?)?;
        let host = url.host_str().ok_or("missing evidence host")?;
        let official = [
            "openai.com",
            "developers.openai.com",
            "platform.openai.com",
            "help.openai.com",
            "anthropic.com",
            "docs.anthropic.com",
            "platform.claude.com",
            "aistudio.google.com",
            "ai.google.dev",
            "cloud.google.com",
            "docs.cloud.google.com",
            "deepmind.google",
            "openrouter.ai",
            "deepinfra.com",
            "docs.x.ai",
            "docs.mistral.ai",
            "api-docs.deepseek.com",
            "docs.cohere.com",
        ];
        require(
            url.scheme() == "https"
                && official.contains(&host)
                && url.username().is_empty()
                && url.password().is_none()
                && url.port().is_none()
                && url
                    .query_pairs()
                    .all(|(key, value)| key == "currency" && value == "usd")
                && entry["checked_on"]
                    .as_str()
                    .ok_or("missing date")?
                    .parse::<NaiveDate>()?
                    <= today,
            format_args!("{name}: evidence must be dated, public official HTTPS"),
        )?;
    }
    fn refs(value: &Value, definitions: &serde_json::Map<String, Value>) -> Validation {
        if let Some(object) = value.as_object() {
            for (key, node) in object {
                if key == "evidence"
                    && let Some(items) = node.as_array()
                {
                    require(
                        items
                            .iter()
                            .all(|item| item.as_str().is_some_and(|r| definitions.contains_key(r))),
                        "missing evidence reference",
                    )?;
                } else {
                    refs(node, definitions)?;
                }
            }
        } else if let Some(items) = value.as_array() {
            for item in items {
                refs(item, definitions)?;
            }
        }
        Ok(())
    }
    refs(value, definitions)
}

#[derive(Clone, Copy)]
struct Route {
    family: &'static str,
    provider: &'static str,
    origin: &'static str,
    path: &'static str,
    wire: &'static str,
    auth: &'static str,
    transport: &'static str,
    kind: OperationKind,
    usage_rule: &'static str,
    bounds_rule: &'static str,
    executable: bool,
}

fn registry(profile: &Profile, kind: OperationKind, binding: &Binding, model: &str) -> Validation {
    use OperationKind::*;
    let routes = [
        (
            "openai_responses",
            "openai",
            "api.openai.com",
            "/v1/responses",
            "v1",
            "bearer",
            "https",
            Completion,
            "responses",
            "responses",
            true,
        ),
        (
            "openai_images",
            "openai",
            "api.openai.com",
            "/v1/images/generations",
            "v1",
            "bearer",
            "https",
            RasterGenerate,
            "sunburst_direct_generate",
            "sunburst_raster",
            true,
        ),
        (
            "openai_images",
            "openai",
            "api.openai.com",
            "/v1/images/edits",
            "v1",
            "bearer",
            "https",
            RasterEdit,
            "sunburst_direct_edit",
            "sunburst_raster",
            true,
        ),
        (
            "openai_images",
            "openai",
            "api.openai.com",
            "/v1/images/generations",
            "v1",
            "bearer",
            "https",
            SvgGenerate,
            "images_svg",
            "images_svg",
            false,
        ),
        (
            "gemini_generate",
            "gemini",
            "generativelanguage.googleapis.com",
            "/v1beta/models/{model}:generateContent",
            "v1beta",
            "api_key_header",
            "https",
            Completion,
            "generate_content",
            "generate_content",
            true,
        ),
        (
            "gemini_generate",
            "gemini",
            "generativelanguage.googleapis.com",
            "/v1beta/models/{model}:generateContent",
            "v1beta",
            "api_key_header",
            "https",
            Completion,
            "flash_lite_generate",
            "flash_lite_generate",
            false,
        ),
        (
            "gemini_generate",
            "gemini",
            "generativelanguage.googleapis.com",
            "/v1beta/models/{model}:generateContent",
            "v1beta",
            "api_key_header",
            "https",
            RasterGenerate,
            "generate_image",
            "generate_image",
            false,
        ),
        (
            "gemini_generate",
            "gemini",
            "generativelanguage.googleapis.com",
            "/v1beta/models/{model}:generateContent",
            "v1beta",
            "api_key_header",
            "https",
            RasterEdit,
            "generate_image",
            "generate_image",
            false,
        ),
        (
            "openrouter_chat",
            "openrouter",
            "openrouter.ai",
            "/api/v1/chat/completions",
            "v1",
            "bearer",
            "https",
            Completion,
            "llama_chat",
            "llama_chat",
            true,
        ),
        (
            "anthropic_messages",
            "anthropic",
            "api.anthropic.com",
            "/v1/messages",
            "2023-06-01",
            "api_key_header",
            "https",
            Completion,
            "haiku_messages",
            "haiku",
            false,
        ),
        (
            "anthropic_batches",
            "anthropic",
            "api.anthropic.com",
            "/v1/messages/batches",
            "2023-06-01",
            "api_key_header",
            "lro",
            Completion,
            "batch_haiku",
            "haiku",
            false,
        ),
        (
            "deepseek_speech",
            "deepseek",
            "api.deepseek.com",
            "/v1/tools/tts",
            "v1",
            "bearer",
            "https",
            SpeechSynthesize,
            "tts",
            "tts",
            false,
        ),
        (
            "deepseek_speech",
            "deepseek",
            "api.deepseek.com",
            "/v1/tools/asr",
            "v1",
            "bearer",
            "https",
            Transcribe,
            "asr",
            "asr",
            false,
        ),
        (
            "xai_video",
            "xai",
            "api.x.ai",
            "/v1/videos/generations",
            "v1",
            "bearer",
            "lro",
            VideoGenerate,
            "video_generate",
            "video_generate",
            false,
        ),
    ];
    let route = routes
        .into_iter()
        .map(
            |(
                family,
                provider,
                origin,
                path,
                wire,
                auth,
                transport,
                kind,
                usage_rule,
                bounds_rule,
                executable,
            )| {
                Route {
                    family,
                    provider,
                    origin,
                    path,
                    wire,
                    auth,
                    transport,
                    kind,
                    usage_rule,
                    bounds_rule,
                    executable,
                }
            },
        )
        .find(|r| {
            r.kind == kind
                && r.family == profile.api_family
                && r.provider == profile.provider
                && profile.origin.strip_prefix("https://") == Some(r.origin)
                && r.path == profile.path
                && r.auth == profile.auth
                && r.wire == profile.wire_version
                && r.transport == profile.transport
                && binding.usage_rule == format!("{}.{}@v1", r.provider, r.usage_rule)
                && binding.bounds_rule == format!("{}.{}@v1", r.provider, r.bounds_rule)
        })
        .ok_or_else(|| format!("{model}: unregistered profile or wire rules"))?;
    require(
        binding.status != "verified"
            || (route.executable
                && (route.provider != "gemini" || model == "gemini/gemini-2.5-flash-lite")),
        format_args!("{model}: no enforceable wire bound"),
    )?;
    let (_, slug) = model.split_once('/').ok_or("missing provider namespace")?;
    require(
        binding.request_model.as_deref() == Some(slug)
            || (route.provider == "deepseek" && binding.request_model.is_none()),
        format_args!("{model}: wire model differs from canonical ID"),
    )?;
    let allowed = match (route.provider, kind) {
        // The catalog owns model IDs within the GPT-6 Responses wire contract.
        ("openai", Completion) => {
            (slug.starts_with("gpt-6-") || slug.starts_with("gpt-6."))
                && !slug.contains("embedding")
        }
        ("openai", _) => matches!(slug, "gpt-image-2.6-sunburst" | "gpt-image-2.6-flare"),
        ("gemini", Completion) => matches!(slug, "gemini-2.5-flash-lite" | "gemini-3.5-flash-lite"),
        ("gemini", _) => slug == "gemini-3.1-flash-lite-image",
        ("openrouter", Completion) => slug == "meta-llama/llama-3.1-8b-instruct",
        ("anthropic", Completion) => slug == "claude-haiku-5-5",
        ("deepseek", SpeechSynthesize) => slug == "tts",
        ("deepseek", Transcribe) => slug == "asr",
        ("xai", VideoGenerate) => slug == "grok-imagine-video-1.5-lite",
        _ => false,
    };
    require(
        allowed,
        format_args!("{model}: wire model is not registered for this operation"),
    )?;
    require(
        binding.response_aliases.is_empty(),
        format_args!("{model}: no independently verified response aliases are registered"),
    )
}

fn validators<'a>(conditions: impl IntoIterator<Item = &'a Condition>) -> Validation {
    for condition in conditions {
        for predicate in condition.values() {
            if let Predicate::Range {
                min,
                max_exclusive: Some(end),
            } = predicate
            {
                require(min < end, "empty condition range")?;
            }
        }
    }
    Ok(())
}

fn validate_selector(selector: &Selector) -> Validation {
    validators([&selector.when])?;
    if let Some(period) = &selector.effective {
        let start = timestamp(&period.from).ok_or("invalid effective date")?;
        if let Some(end) = &period.until {
            require(
                timestamp(end).is_some_and(|date| date > start),
                "invalid effective period",
            )?;
        }
    }
    Ok(())
}

pub(crate) fn fixed_cell(profile: &Profile, binding: &Binding, effect: &Effect) -> Cell {
    let mut cell = Cell::from([
        ("tier".into(), json!(effect.effective_tier)),
        ("mode".into(), json!(effect.mode)),
        ("plan".into(), json!(binding.plan)),
        ("residency".into(), json!(binding.residency)),
    ]);
    if let Some(region) = effect
        .region
        .as_ref()
        .or(binding.region.as_ref())
        .or(profile.region.as_ref())
    {
        cell.insert("region".into(), json!(region));
    }
    cell
}

pub(crate) fn cells(
    profile: &PriceProfile,
    fixed: &Cell,
    constrains: &Condition,
    time_bands: Option<Vec<Value>>,
) -> Validation<Vec<Cell>> {
    let selectors = profile
        .components
        .values()
        .flat_map(|c| c.rates.iter().map(|r| &r.selector))
        .chain(
            profile
                .factor_groups
                .values()
                .flat_map(|g| g.rows.iter().map(|r| &r.selector)),
        );
    let mut axes = Map::<Vec<Value>>::new();
    let mut observe = |condition: &Condition| {
        for (axis, test) in condition {
            if fixed.contains_key(axis) {
                continue;
            }
            let values = axes
                .entry(axis.clone())
                .or_insert_with(|| vec![Value::Null]);
            match test {
                Predicate::OneOf(allowed) => values.extend(allowed.iter().cloned()),
                Predicate::Range { min, max_exclusive } => {
                    values.extend([json!(0), json!(min)]);
                    values.extend(max_exclusive.iter().map(|end| json!(end)));
                }
            }
        }
    };
    observe(constrains);
    for c in profile.components.values() {
        observe(&c.applies);
    }
    for fee in profile.fees.values() {
        observe(&fee.applies);
    }
    let mut instants = BTreeSet::new();
    for selector in selectors {
        observe(&selector.when);
        if let Some(period) = &selector.effective {
            instants.insert(timestamp(&period.from).ok_or("invalid time")?);
            if let Some(end) = &period.until {
                instants.insert(timestamp(end).ok_or("invalid time")?);
            }
        }
    }
    if let Some(first) = instants.first().copied() {
        instants.insert(first - 1);
        let dates = instants.into_iter().map(|ms| {
            let time = chrono::DateTime::from_timestamp_millis(ms).expect("bounded effective date");
            json!(time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        });
        axes.insert("effective".into(), dates.collect());
    }
    if axes.contains_key("time_band") {
        axes.insert(
            "time_band".into(),
            time_bands.ok_or("time_band without time_window")?,
        );
    }
    let mut cells = vec![fixed.clone()];
    for (axis, mut values) in axes {
        if values.iter().any(Value::is_number) {
            values.retain(Value::is_number);
        } else if matches!(
            axis.as_str(),
            "promotion" | "has_references" | "audio_enabled"
        ) {
            values = vec![json!(false), json!(true)];
        } else if values.iter().any(Value::is_null) && matches!(axis.as_str(), "outcome" | "state")
        {
            let possible: &[&str] = if axis == "outcome" {
                &["success", "filtered", "partial", "failed", "cancelled"]
            } else {
                &["active", "standby", "running", "idle"]
            };
            values = possible.iter().map(|v| json!(v)).collect();
        }
        values.sort_by_cached_key(Value::to_string);
        values.dedup();
        let restricted = constrains.get(&axis);
        let values: Vec<_> = values
            .into_iter()
            .filter(|v| restricted.is_none_or(|p| p.contains(v)))
            .collect();
        require(
            cells.len().saturating_mul(values.len()) <= 1_000_000,
            "condition partition exceeds verifier limit",
        )?;
        cells = cells
            .into_iter()
            .flat_map(|cell| {
                let axis = &axis;
                values.iter().map(move |value| {
                    let mut cell = cell.clone();
                    cell.insert(axis.clone(), value.clone());
                    cell
                })
            })
            .collect();
    }
    cells.retain(|cell| matches(constrains, cell));
    require(
        !cells.is_empty(),
        "pricing has no reachable condition cells",
    )?;
    Ok(cells)
}

fn validate_price(catalog: &Catalog, id: &str, price: &PriceProfile) -> Validation {
    if let Some(window) = &price.time_window {
        require(
            catalog
                .time_windows
                .get(window)
                .is_some_and(|w| w.clock == price.clock),
            format_args!("{id}: time window or clock"),
        )?;
    }
    if let Some(rule) = &price.charge_rule {
        decimal(&rule.minimum)?;
        decimal(&rule.quantum)?;
    }
    for component in price.components.values() {
        validators([&component.applies])?;
        let mut axes = BTreeSet::new();
        for rule in &component.quantization {
            require(
                axes.insert(rule.axis),
                format_args!("{id}: duplicated quantization axis"),
            )?;
            require(
                match (component.meter.unit, component.meter.time_basis.as_deref()) {
                    (Unit::SizeMs, _) => matches!(rule.axis, Axis::Size | Axis::Duration),
                    (Unit::ResourceTime, Some("elapsed_ms")) => {
                        matches!(rule.axis, Axis::Count | Axis::Duration)
                    }
                    _ => rule.axis == Axis::Quantity,
                },
                format_args!("{id}: quantization axis and unit"),
            )?;
        }
        if let Some(allowance) = &component.allowance {
            require(
                catalog
                    .allowances
                    .get(allowance)
                    .is_some_and(|a| a.kind == "included_usage" && !a.amount.contains('.')),
                "invalid included usage allowance",
            )?;
        }
        for rate in &component.rates {
            validate_selector(&rate.selector)?;
            if let Some(amount) = &rate.amount {
                decimal(amount)?;
            }
            if let Some(bands) = &rate.marginal {
                let mut start = Some("0");
                for band in bands {
                    require(
                        start == Some(band.quantity.min.as_str()),
                        format_args!("{id}: marginal bands must be contiguous from zero"),
                    )?;
                    start = band.quantity.max_exclusive.as_deref();
                    if let Some(end) = start {
                        require(
                            decimal(end)? > decimal(&band.quantity.min)?,
                            "empty marginal band",
                        )?;
                    }
                    decimal(&band.amount)?;
                }
                require(start.is_none(), "marginal bands need an uncapped last band")?;
            }
        }
    }
    for group in price.factor_groups.values() {
        require(
            group
                .components
                .iter()
                .all(|id| price.components.contains_key(id)),
            "unknown factor component",
        )?;
        for row in &group.rows {
            validate_selector(&row.selector)?;
            decimal(&row.factor)?;
        }
    }
    for fee in price.fees.values() {
        validators([&fee.applies])?;
        decimal(&fee.factor)?;
        require(
            catalog
                .price_profiles
                .get(&fee.base_profile)
                .is_some_and(|base| {
                    base.currency == price.currency
                        && base.billing_provider == price.billing_provider
                }),
            format_args!("{id}: invalid fee base"),
        )?;
        if let Some(allowance) = &fee.allowance {
            require(
                catalog
                    .allowances
                    .get(allowance)
                    .is_some_and(|a| a.kind == "included_money"),
                "invalid included money allowance",
            )?;
        }
    }
    Ok(())
}

fn check_cells(price: &PriceProfile, cells: &[Cell], verified: bool, id: &str) -> Validation {
    for cell in cells {
        for (name, component) in &price.components {
            if !matches(&component.applies, cell) {
                continue;
            }
            let context = || format!("{id}/{name}: pricing for {cell:?}");
            let rate =
                unique(&component.rates, |r| r.selector.matches(cell)).map_err(|_| context())?;
            require(
                !verified || (rate.state == RateState::Priced && component.rounding_known),
                context(),
            )?;
            for group in price
                .factor_groups
                .values()
                .filter(|g| g.components.contains(name))
            {
                unique(&group.rows, |r| r.selector.matches(cell)).map_err(|_| context())?;
            }
        }
    }
    Ok(())
}

fn check_meter(meter: &Meter, op: &Operation, profile: &Profile, tool: Option<&str>) -> bool {
    use OperationKind::*;
    let modalities = match meter.direction {
        Direction::Input => &op.input,
        Direction::Output => &op.output,
        Direction::Neutral => {
            return match meter.unit {
                Unit::Request => match meter.item_kind.as_deref() {
                    Some("grounded_prompt") => {
                        profile.api_family.starts_with("gemini_") && tool.is_some()
                    }
                    None => matches!(op.kind, Completion | Search | Fetch),
                    _ => false,
                },
                Unit::ToolCall | Unit::SearchQuery => {
                    tool.is_some_and(|name| {
                        meter.item_kind.as_deref() == Some(name)
                            || (name == "google_search"
                                && meter.item_kind.as_deref() == Some("web_search"))
                    }) || (op.kind == VoiceSession
                        && meter.item_kind.as_deref() == Some("text_message"))
                }
                Unit::Record => profile.provider == "xai" && tool == Some("x_search"),
                Unit::ResourceTime => {
                    op.kind == VoiceSession
                        || matches!(tool, Some("code_execution" | "hosted_shell"))
                }
                _ => false,
            };
        }
    };
    if meter
        .modalities
        .as_ref()
        .is_some_and(|m| m.iter().any(|value| !modalities.contains(value)))
    {
        return false;
    }
    match meter.unit {
        Unit::Token => true,
        Unit::Image | Unit::Pixel => matches!(
            op.kind,
            RasterGenerate | RasterEdit | ImageUpscale | VideoGenerate | VideoEdit
        ),
        Unit::Artifact => op.kind == MusicGenerate,
        Unit::MediaMs => matches!(
            op.kind,
            Completion
                | Transcribe
                | AudioTranslate
                | VoiceSession
                | SpeechSynthesize
                | VideoGenerate
                | VideoEdit
                | VideoExtend
        ),
        Unit::Character => matches!(op.kind, Completion | SpeechSynthesize),
        Unit::Page => matches!(op.kind, Ocr | Parse),
        _ => false,
    }
}

fn check_capabilities(op: &Operation, binding: &Binding, profile: &Profile) -> Validation {
    use OperationKind::*;
    let (input, output): (&[&str], &[&str]) = match op.kind {
        Completion if profile.provider == "openrouter" => (&["text"], &["text", "structured"]),
        Completion => (&["text", "image", "document"], &["text", "structured"]),
        RasterGenerate => (&["text"], &["image", "text"]),
        RasterEdit => (&["text", "image"], &["image", "text"]),
        SvgGenerate => (&["text"], &["image"]),
        SpeechSynthesize => (&["text"], &["audio"]),
        Transcribe => (&["audio"], &["text"]),
        VideoGenerate => (&["text", "image", "video"], &["video", "audio"]),
        _ => return Err("no wire capability mapping registered for operation".into()),
    };
    require(
        op.input.iter().all(|m| input.contains(&m.as_str()))
            && op.output.iter().all(|m| output.contains(&m.as_str())),
        "modality incompatible with registered operation",
    )?;
    if binding.status == "verified" {
        require(
            op.capabilities.tools.iter().all(|kind| kind == "function")
                && (profile.api_family == "openai_images"
                    || (op.capabilities.image_qualities.is_empty()
                        && op.capabilities.image_sizes.is_empty())),
            "unsupported verified wire capability",
        )?;
        if profile.api_family == "openai_responses" {
            require(
                matches!(binding.constrains.get("retention_seconds"), Some(Predicate::Range { min, max_exclusive })
                    if matches!(min, 1800 | 43200) && *max_exclusive == min.checked_add(1)
                        && op.capabilities.cache_write_seconds.contains(min)),
                "explicit supported cache TTL is required",
            )?;
        }
    }
    Ok(())
}

fn check_raster_reservation(op: &Operation, binding: &Binding) -> Validation {
    let rule = binding
        .image_reservation
        .as_ref()
        .ok_or("missing raster reservation")?;
    let limits = &binding.limits;
    let edit = op.kind == OperationKind::RasterEdit;
    let max_output = rule
        .calculator_output_tokens
        .values()
        .flat_map(|sizes| sizes.values())
        .copied()
        .max()
        .and_then(|n| n.checked_mul(rule.output_multiplier));
    require(
        matches!(
            op.kind,
            OperationKind::RasterGenerate | OperationKind::RasterEdit
        ) && op.capabilities.formats == ["png"]
            && !op.capabilities.auto_size
            && op.capabilities.image_sizes.contains(&"1024x1024".into())
            && op.capabilities.image_qualities.contains(&"low".into())
            && rule.calculator_output_tokens.len() == op.capabilities.image_sizes.len()
            && op.capabilities.image_sizes.iter().all(|size| {
                rule.calculator_output_tokens.get(size).is_some_and(|row| {
                    row.len() == op.capabilities.image_qualities.len()
                        && op
                            .capabilities
                            .image_qualities
                            .iter()
                            .all(|quality| row.contains_key(quality))
                })
            })
            && rule.output_multiplier >= 2
            && rule.prompt_bytes as u64 <= rule.text_tokens
            && max_output.is_some()
            && limits.max_output_tokens == max_output
            && limits.outputs == Some(1)
            && limits.images_in == edit.then_some(1)
            && rule.reference_long_edge <= 1536
            && rule.reference_tokens >= 14_048
            && limits.max_input_tokens
                == rule
                    .text_tokens
                    .checked_add(if edit { rule.reference_tokens } else { 0 })
            && limits.context_tokens
                == limits
                    .max_input_tokens
                    .zip(max_output)
                    .and_then(|(i, o)| i.checked_add(o)),
        "raster limits must cover the calculator reservation",
    )
}

fn check_raster_components(price: &PriceProfile, kind: OperationKind) -> Validation {
    let mut expected = vec![
        ("text-in", Direction::Input, "text", "base"),
        ("text-cache", Direction::Input, "text", "cache_read"),
        ("image-out", Direction::Output, "image", "base"),
    ];
    if kind == OperationKind::RasterEdit {
        expected.extend([
            ("image-in", Direction::Input, "image", "base"),
            ("image-cache", Direction::Input, "image", "cache_read"),
        ]);
    }
    for (base, cache) in [("text-in", "text-cache"), ("image-in", "image-cache")] {
        if let Some(cached) = price.components.get(cache) {
            let rate = |component: &Component| -> Result<_, Box<dyn std::error::Error>> {
                let rate = component.rates.first().ok_or("missing raster rate")?;
                Ok(
                    decimal(rate.amount.as_deref().ok_or("unknown raster rate")?)?
                        / decimal(
                            rate.per
                                .as_deref()
                                .ok_or("missing raster rate denominator")?,
                        )?,
                )
            };
            require(
                rate(
                    price
                        .components
                        .get(base)
                        .ok_or("missing uncached raster meter")?,
                )? >= rate(cached)?,
                "raster reservation requires undiscounted rates to bound cached rates",
            )?;
        }
    }

    require(
        price.factor_groups.is_empty()
            && price.components.len() == expected.len()
            && expected
                .into_iter()
                .all(|(id, direction, modality, class)| {
                    price.components.get(id).is_some_and(|c| {
                        c.meter.direction == direction
                            && c.meter.class.as_deref() == Some(class)
                            && c.meter
                                .modalities
                                .as_deref()
                                .is_some_and(|m| m == [modality])
                            && c.applies.is_empty()
                            && c.rates.len() == 1
                            && c.rates[0].selector == Selector::default()
                            && c.rates[0].marginal.is_none()
                    })
                }),
        "unsupported direct raster price meters",
    )
}

fn check_verified_components(price: &PriceProfile, profile: &Profile) -> Validation {
    let expected: &[(&str, Direction, Option<u64>)] = if profile.provider == "openai" {
        &[
            ("base", Direction::Input, None),
            ("cache_read", Direction::Input, None),
            ("cache_write", Direction::Input, Some(1800)),
            ("cache_write", Direction::Input, Some(43200)),
            ("base", Direction::Output, None),
        ]
    } else if profile.api_family == "openrouter_chat" {
        &[
            ("base", Direction::Input, None),
            ("base", Direction::Output, None),
        ]
    } else {
        &[
            ("base", Direction::Input, None),
            ("cache_read", Direction::Input, None),
            ("base", Direction::Output, None),
        ]
    };
    let conditions = price
        .components
        .values()
        .flat_map(|c| {
            std::iter::once(&c.applies).chain(c.rates.iter().map(|rate| &rate.selector.when))
        })
        .chain(
            price
                .factor_groups
                .values()
                .flat_map(|g| g.rows.iter().map(|row| &row.selector.when)),
        );
    let known = [
        "prompt_tokens",
        "context_tokens",
        "retention_seconds",
        "tier",
        "mode",
        "plan",
        "residency",
    ];
    for condition in conditions {
        require(
            condition.keys().all(|key| known.contains(&key.as_str())),
            "price condition unavailable on the wire",
        )?;
    }
    require(
        price.components.len() == expected.len()
            && expected.iter().all(|&(class, direction, retention)| {
                price
                    .components
                    .values()
                    .filter(|c| {
                        c.meter.class.as_deref() == Some(class)
                            && c.meter.direction == direction
                            && c.meter.retention_seconds == retention
                    })
                    .count()
                    == 1
            })
            && price.components.values().all(|component| {
                component.applies.is_empty()
                    && component
                        .rates
                        .iter()
                        .all(|r| r.marginal.is_none() && r.selector.effective.is_none())
            })
            && price
                .factor_groups
                .values()
                .flat_map(|g| &g.rows)
                .all(|r| r.selector.effective.is_none()),
        "verified price meters must partition the counters of this adapter",
    )
}

fn check_byok_prices(catalog: &Catalog, effect: &Effect, profile: &Profile) -> Validation {
    require(
        effect.prices.len() == 2,
        "BYOK requires upstream and router price profiles",
    )?;
    let upstream = catalog
        .price_profiles
        .get(&effect.prices[0])
        .ok_or("missing upstream price")?;
    let router = catalog
        .price_profiles
        .get(&effect.prices[1])
        .ok_or("missing router fee")?;
    let fee = router.fees.get("platform").ok_or("missing platform fee")?;
    let reference = catalog
        .price_profiles
        .get(&fee.base_profile)
        .ok_or("missing list price")?;
    require(
        upstream.payer == "upstream"
            && upstream.fees.is_empty()
            && router.payer == "router"
            && router.components.is_empty()
            && router.fees.len() == 1
            && fee.applies.is_empty()
            && fee.factor == "0.05"
            && fee.allowance.is_some()
            && reference.purpose == "reference"
            && reference.payer == "router"
            && reference.fees.is_empty()
            && reference.charge_rule.is_none()
            && reference.time_window.is_none()
            && catalog
                .price_profiles
                .get("router-deepinfra-llama")
                .is_some_and(|routed| routed.components == reference.components)
            && reference.factor_groups.is_empty()
            && upstream.factor_groups.is_empty()
            && upstream.components.keys().eq(reference.components.keys())
            && upstream
                .components
                .values()
                .zip(reference.components.values())
                .all(|(u, r)| u.meter == r.meter),
        "unsupported OpenRouter BYOK accounting",
    )?;
    check_verified_components(upstream, profile)
}

struct Use<'a> {
    op: &'a Operation,
    binding: &'a Binding,
    profile: &'a Profile,
    effect: &'a Effect,
}

fn visit_price(
    catalog: &Catalog,
    id: &str,
    usage: &Use<'_>,
    tool: Option<&str>,
    visiting: &mut BTreeSet<String>,
    used: &mut BTreeSet<String>,
) -> Validation {
    let price = catalog
        .price_profiles
        .get(id)
        .ok_or_else(|| format!("unknown price {id}"))?;
    require(
        visiting.insert(id.into()),
        format_args!("{id}: cyclic fee reference"),
    )?;
    used.insert(id.into());
    let mut origins = BTreeSet::new();
    for component in price.components.values() {
        require(
            check_meter(&component.meter, usage.op, usage.profile, tool),
            format_args!("{id}: meter incompatible with operation"),
        )?;
        let mut meter = component.meter.clone();
        if let Some(values) = &mut meter.modalities {
            values.sort();
        }
        require(
            origins.insert(serde_json::to_string(&meter)?),
            format_args!("{id}: duplicate meter"),
        )?;
        if let Some(seconds) = meter.retention_seconds {
            require(
                usage.op.capabilities.cache_write_seconds.contains(&seconds),
                format_args!("{id}: unavailable retention"),
            )?;
        }
    }
    let window = price
        .time_window
        .as_ref()
        .map(|w| catalog.time_windows[w].bands());
    let mut domain = usage.binding.constrains.clone();
    let caps = &usage.op.capabilities;
    let resolution: Vec<_> = caps
        .video_resolutions
        .iter()
        .chain(&caps.image_sizes)
        .map(|s| json!(s))
        .collect();
    for (axis, values) in [
        ("resolution", resolution),
        (
            "quality",
            caps.image_qualities.iter().map(|s| json!(s)).collect(),
        ),
        (
            "media_mode",
            caps.media_modes.iter().map(|s| json!(s)).collect(),
        ),
        (
            "audio_enabled",
            caps.video_audio.iter().map(|s| json!(s)).collect(),
        ),
        (
            "duration_ms",
            caps.duration_ms_options.iter().map(|s| json!(s)).collect(),
        ),
    ] {
        if !values.is_empty() {
            let values: Vec<_> = values
                .into_iter()
                .filter(|value| {
                    domain
                        .get(axis)
                        .is_none_or(|condition| condition.contains(value))
                })
                .collect();
            require(
                !values.is_empty(),
                format_args!("{id}: condition selects no {axis}"),
            )?;
            domain.insert(axis.into(), Predicate::OneOf(values));
        }
    }
    let cells = cells(
        price,
        &fixed_cell(usage.profile, usage.binding, usage.effect),
        &domain,
        window,
    )?;
    check_cells(price, &cells, usage.binding.status == "verified", id)?;
    for fee in price.fees.values() {
        visit_price(catalog, &fee.base_profile, usage, tool, visiting, used)?;
    }
    visiting.remove(id);
    Ok(())
}

fn co_billed(catalog: &Catalog, effect: &Effect, visiting: &mut BTreeSet<String>) -> Validation {
    for next in effect.co_billed.values() {
        let model = catalog
            .models
            .get(&next.model)
            .ok_or("unknown co-billed model")?;
        require(
            model.operations.contains(&next.operation),
            "co-billed operation ownership",
        )?;
        validators([&next.applies])?;
        let next_effect = catalog
            .operations
            .get(&next.operation)
            .and_then(|o| o.bindings.get(&next.binding))
            .and_then(|b| b.effects.get(&next.effect))
            .ok_or("co-billed binding/effect does not exist")?;
        let key = format!("{}/{}/{}", next.operation, next.binding, next.effect);
        require(visiting.insert(key.clone()), "cyclic co-billed operation")?;
        co_billed(catalog, next_effect, visiting)?;
        visiting.remove(&key);
    }
    Ok(())
}

fn validate(catalog: &Catalog) -> Validation {
    let mut operations = BTreeSet::new();
    let mut profiles = BTreeSet::new();
    let mut prices = BTreeSet::new();
    for (id, window) in &catalog.time_windows {
        window.validate(id)?;
    }
    for allowance in catalog.allowances.values() {
        decimal(&allowance.amount)?;
        require(
            allowance
                .zone
                .as_ref()
                .is_none_or(|z| z.parse::<chrono_tz::Tz>().is_ok()),
            "invalid allowance zone",
        )?;
        require(
            allowance.period != "continuous" || allowance.scope == "resource",
            "continuous allowance must be resource scoped",
        )?;
    }
    for (id, price) in &catalog.price_profiles {
        validate_price(catalog, id, price)?;
    }
    for (id, model) in &catalog.models {
        let (provider, _) = id.split_once('/').ok_or("missing provider namespace")?;
        let mut kinds = Vec::new();
        for op_id in &model.operations {
            let op = catalog.operations.get(op_id).ok_or("unknown operation")?;
            require(
                operations.insert(op_id) && !kinds.contains(&op.kind),
                format_args!("{id}: non-unique operation"),
            )?;
            kinds.push(op.kind);
            require(
                op.bindings.contains_key(&op.default_binding),
                format_args!("{id}: missing default binding"),
            )?;
            for (name, binding) in &op.bindings {
                let profile = catalog
                    .profiles
                    .get(&binding.profile)
                    .ok_or("unknown connection profile")?;
                profiles.insert(&binding.profile);
                require(
                    provider == profile.provider,
                    format_args!("{id}: provider mismatch"),
                )?;
                registry(profile, op.kind, binding, id)?;
                if let Some(policy) = &binding.cache_policy {
                    require(
                        profile.api_family == "openai_images"
                            && matches!(
                                op.kind,
                                OperationKind::RasterGenerate | OperationKind::RasterEdit
                            )
                            && matches!(
                                id.as_str(),
                                "openai/gpt-image-2.6-flare" | "openai/gpt-image-2.6-sunburst"
                            )
                            && binding
                                .effects
                                .values()
                                .all(|effect| effect.mode == "direct")
                            && policy
                                .evidence
                                .iter()
                                .any(|id| id == "openai-image-2-2-5-direct-cache"),
                        "inherited cache policy is restricted to direct 2.6 raster bindings and requires 2/2.5 evidence",
                    )?;
                }
                require(
                    !binding.background
                        || (profile.api_family == "openai_responses"
                            && id == "openai/gpt-6.1-terra"
                            && op.kind == OperationKind::Completion),
                    "binding has no registered background request and polling contract",
                )?;
                require(
                    binding.limits.image_output_tokens_by_size.is_empty()
                        || (matches!(
                            op.kind,
                            OperationKind::RasterGenerate | OperationKind::RasterEdit
                        ) && binding.limits.max_output_tokens.is_some_and(|max| {
                            binding
                                .limits
                                .image_output_tokens_by_size
                                .values()
                                .all(|&tokens| tokens > 0 && tokens <= max)
                        })),
                    "fixed image output token quantities need a raster token bound",
                )?;
                check_capabilities(op, binding, profile)?;
                validators([&binding.constrains])?;
                if binding.status == "verified" {
                    let limits = &binding.limits;
                    if profile.api_family == "openai_images" {
                        check_raster_reservation(op, binding)?;
                    } else {
                        require(
                            binding.image_reservation.is_none(),
                            "unexpected raster reservation",
                        )?;
                    }
                    require(
                        !binding.allow_fallbacks
                            && (binding.effects.len() == 1
                                || (profile.api_family == "openrouter_chat"
                                    && op.kind == OperationKind::Completion
                                    && binding.effects.len() == 2
                                    && limits.context_tokens.is_some_and(|n| n <= 32000)
                                    && binding.effects.values().all(|e| e.mode == "routed")
                                    && binding
                                        .effects
                                        .values()
                                        .map(|e| &e.upstream_provider)
                                        .collect::<BTreeSet<_>>()
                                        .len()
                                        == 2))
                            && binding.requested_tier == "standard"
                            && binding.plan == "paid"
                            && binding.residency == "global"
                            && !op.capabilities.stream
                            && limits
                                .max_output_tokens
                                .zip(limits.context_tokens)
                                .is_some_and(|(o, c)| {
                                    o <= c && limits.max_input_tokens.is_none_or(|i| i <= c)
                                })
                            && limits.request_bytes.is_some()
                            && limits.response_bytes.is_some(),
                        format_args!("{id}/{name}: unsupported executable bounds"),
                    )?;
                }
                for effect in binding.effects.values() {
                    let usage = Use {
                        op,
                        binding,
                        profile,
                        effect,
                    };
                    require(
                        effect.mode != "direct"
                            || (effect.endpoint_tag.is_none()
                                && effect.upstream_provider.is_none()),
                        "direct effect specifies router",
                    )?;
                    for price_id in &effect.prices {
                        let price = catalog
                            .price_profiles
                            .get(price_id)
                            .ok_or("unknown effect price")?;
                        require(
                            price.purpose == "billable",
                            "reference price cannot be charged directly",
                        )?;
                        let payer = match effect.mode.as_str() {
                            "direct" => {
                                price.payer == "direct"
                                    && price.billing_provider == profile.provider
                            }
                            "routed" => {
                                price.payer == "router"
                                    && price.billing_provider == profile.provider
                            }
                            "byok" => {
                                (price.payer == "router"
                                    && price.billing_provider == profile.provider)
                                    || (price.payer == "upstream"
                                        && effect.upstream_provider.as_ref()
                                            == Some(&price.billing_provider))
                            }
                            _ => false,
                        };
                        require(payer, format_args!("{id}: billing payer mismatch"))?;
                        if binding.status == "verified" {
                            require(
                                (effect.mode == "direct"
                                    || (profile.api_family == "openrouter_chat"
                                        && matches!(effect.mode.as_str(), "routed" | "byok")
                                        && match (
                                            effect.upstream_provider.as_deref(),
                                            effect.endpoint_tag.as_deref(),
                                        ) {
                                            (Some("deepinfra"), Some("deepinfra/fp8")) => true,
                                            (Some("cloudflare"), Some("cloudflare/fp8")) => {
                                                effect.mode == "routed"
                                                    && binding
                                                        .limits
                                                        .context_tokens
                                                        .is_some_and(|n| n <= 32000)
                                            }
                                            _ => false,
                                        }))
                                    && effect.effective_tier == binding.requested_tier
                                    && price.currency == "USD"
                                    && (effect.mode == "byok" || effect.prices.len() == 1)
                                    && effect.tool_prices.is_empty()
                                    && effect.co_billed.is_empty()
                                    && effect.cache_lifecycle.is_none()
                                    && (effect.mode == "byok" || price.fees.is_empty())
                                    && price.charge_rule.is_none()
                                    && price.time_window.is_none()
                                    && price.components.values().all(|c| {
                                        c.meter.unit == Unit::Token
                                            && c.allowance.is_none()
                                            && c.quantization.is_empty()
                                            && c.bill_on == BillOn::Measured
                                    }),
                                "active adapter cannot meter or bound this price",
                            )?;
                            if effect.mode == "byok" {
                                check_byok_prices(catalog, effect, profile)?;
                            } else if profile.api_family == "openai_images" {
                                check_raster_components(price, op.kind)?;
                            } else {
                                check_verified_components(price, profile)?;
                            }
                        }
                        visit_price(
                            catalog,
                            price_id,
                            &usage,
                            None,
                            &mut BTreeSet::new(),
                            &mut prices,
                        )?;
                    }
                    require(
                        op.capabilities.tools.iter().all(|tool| {
                            tool == "function" || effect.tool_prices.contains_key(tool)
                        }),
                        "provider tool lacks a fee or explicit zero price",
                    )?;
                    for (tool, price_id) in &effect.tool_prices {
                        require(
                            op.capabilities.tools.contains(tool),
                            "tool price for disabled tool",
                        )?;
                        visit_price(
                            catalog,
                            price_id,
                            &usage,
                            Some(tool),
                            &mut BTreeSet::new(),
                            &mut prices,
                        )?;
                    }
                    if let Some(cache) = &effect.cache_lifecycle {
                        let cache_profile = catalog
                            .profiles
                            .get(&cache.profile)
                            .ok_or("unknown cache connection")?;
                        profiles.insert(&cache.profile);
                        let price = catalog
                            .price_profiles
                            .get(&cache.price)
                            .ok_or("unknown cache price")?;
                        prices.insert(cache.price.clone());
                        require(
                            profile.api_family == "gemini_generate"
                                && cache_profile.api_family == "gemini_cache"
                                && profile.provider == cache_profile.provider
                                && cache_profile.origin == profile.origin
                                && cache_profile.path == "/v1beta/cachedContents"
                                && cache_profile.method == "POST"
                                && cache_profile.auth == profile.auth
                                && cache_profile.wire_version == profile.wire_version
                                && cache_profile.region == profile.region
                                && price.payer == "direct"
                                && price.billing_provider == profile.provider
                                && price.purpose == "billable"
                                && price
                                    .components
                                    .values()
                                    .all(|c| c.meter.unit == Unit::SizeMs)
                                && cache.usage_rule == "gemini.cache_usage@v1"
                                && cache.bounds_rule == "gemini.cache_ttl@v1"
                                && cache.max_tokens > 0
                                && cache.max_lifetime_ms > 0,
                            "incompatible cache lifecycle",
                        )?;
                    }
                    co_billed(catalog, effect, &mut BTreeSet::new())?;
                }
            }
        }
    }
    require(
        operations.len() == catalog.operations.len()
            && profiles.len() == catalog.profiles.len()
            && prices.len() == catalog.price_profiles.len(),
        "unreferenced operation, connection, or price",
    )?;
    Ok(())
}

/// Build the canonical embedded artifact from a strictly validated catalog.
pub fn generate(raw: &str, schema: &str, today: &str) -> Validation<(String, String)> {
    let UniqueJson(value) = serde_json::from_str(raw)?;
    let UniqueJson(schema) = serde_json::from_str(schema)?;
    let validator = jsonschema::draft202012::options()
        .should_validate_formats(true)
        .build(&schema)?;
    if let Some(error) = validator.iter_errors(&value).next() {
        // Use the location only. Invalid evidence can contain secrets by accident.
        return Err(format!("catalog schema violation at {}", error.instance_path()).into());
    }
    evidence(&value, today.parse()?)?;
    let catalog: Catalog = serde_json::from_value(value.clone())?;
    validate(&catalog)?;
    let serialized = serde_json::to_vec(&value)?;
    let digest = format!("{:x}", Sha256::digest(&serialized));
    let index = serde_json::to_string(&json!({"catalog": value, "digest": digest}))?;
    let mut listing = format!(
        "<!-- Generated by cargo run -p xtask -- models generate -->\n# Bundled models\n\nRevision `{}`\n\nSHA-256 `{}`\n\n| Model ID | Operation | Binding | Status |\n| --- | --- | --- | --- |\n",
        catalog.catalog_revision, digest
    );
    use std::fmt::Write;
    for (id, model) in &catalog.models {
        for op in &model.operations {
            let operation = &catalog.operations[op];
            let kind = serde_json::to_value(operation.kind)?;
            for (name, binding) in &operation.bindings {
                writeln!(
                    listing,
                    "| `{id}` | `{}` | `{name}` | {} |",
                    kind.as_str().unwrap_or_default(),
                    binding.status
                )?;
            }
        }
    }
    Ok((format!("{index}\n"), listing))
}
