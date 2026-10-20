use std::{
    collections::BTreeMap,
    fs,
    ops::Bound::{Excluded, Unbounded},
    path::Path,
    sync::Arc,
};

use chrono::Utc;
use redb::{
    Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, TableHandle,
    WriteTransaction,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use crate::types::{AlertRecord, AttemptRecord, RequestRecord, UpstreamErrorBucket};

const STATE: TableDefinition<&str, &[u8]> = TableDefinition::new("state");
const REQUESTS: TableDefinition<&str, &[u8]> = TableDefinition::new("requests");
const ATTEMPTS: TableDefinition<&str, &[u8]> = TableDefinition::new("attempts");
const ALERTS: TableDefinition<&str, &[u8]> = TableDefinition::new("alerts");
type RequestRollupValue = (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, u64);
type RequestBucketKey<'a> = (
    &'a str,
    u8,
    i64,
    u8,
    u32,
    &'a str,
    &'a str,
    &'a str,
    &'a str,
);
type RequestAllKey<'a> = (&'a str, u8, u32, &'a str, &'a str, &'a str, &'a str);
type AttemptRollupValue = (u64, u64, u64, u64, u64, u64, u64, u64, u64, u64, f64);

const REQUEST_ROLLUP_BUCKET: TableDefinition<RequestBucketKey<'static>, RequestRollupValue> =
    TableDefinition::new("request_rollup_bucket_v1");
const REQUEST_ROLLUP_ALL: TableDefinition<RequestAllKey<'static>, RequestRollupValue> =
    TableDefinition::new("request_rollup_all_v1");
const ATTEMPT_ROLLUP_ALL: TableDefinition<(&str, &str), AttemptRollupValue> =
    TableDefinition::new("attempt_rollup_all_v1");
type AttemptBucketKey<'a> = (u8, i64, &'a str, &'a str);
const ATTEMPT_ROLLUP_BUCKET: TableDefinition<AttemptBucketKey<'static>, AttemptRollupValue> =
    TableDefinition::new("attempt_rollup_bucket_v1");
const ATTEMPT_BUCKET_META_KEY: &str = "attempt_buckets";

#[derive(Default, Deserialize, Serialize)]
struct AttemptBucketMeta {
    cursor: Option<String>,
    ready: bool,
}

const STATS_META: TableDefinition<&str, &[u8]> = TableDefinition::new("stats_meta_v1");

const STATS_META_KEY: &str = "rollup";
/// Version 2 adds cache-hit, cache-miss and reasoning token detail to both rollups.
/// Version 3 adds locally estimated reasoning tokens for upstreams that omit the
/// reasoning split while still streaming thinking text.
/// Upgrading rebuilds every rollup table from the retained request/attempt ledger.
const STATS_SCHEMA_VERSION: u32 = 3;
const MIGRATION_BATCH_SIZE: usize = 1_000;
const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 3_600_000;
const ROLLUP_RETENTION_MS: i64 = 32 * 24 * HOUR_MS;
const SCOPE_MODEL: u8 = 0;
const SCOPE_TARGET: u8 = 1;
const SCOPE_UNATTRIBUTED: u8 = 2;
const GRAIN_MINUTE: u8 = 0;
const GRAIN_HOUR: u8 = 1;
const UNCLASSIFIED_MODEL: &str = "\0unclassified";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct RequestRollup {
    pub calls: u64,
    pub errors: u64,
    pub fallbacks: u64,
    pub input_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub reasoning_tokens: u64,
    pub reasoning_estimated_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct AttemptRollup {
    pub attempts: u64,
    pub successes: u64,
    pub errors: u64,
    pub cache_reported_attempts: u64,
    pub input_tokens: u64,
    pub cache_hit_tokens: u64,
    pub cache_miss_tokens: u64,
    pub reasoning_tokens: u64,
    pub reasoning_estimated_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RouteRollupKey {
    pub layer_index: u32,
    pub layer_name: String,
    pub backend: String,
    pub credential: String,
    pub upstream_model: String,
}

#[derive(Clone, Debug, Default)]
pub struct RoutingRollup {
    pub total: RequestRollup,
    pub targets: BTreeMap<RouteRollupKey, RequestRollup>,
    pub unattributed: RequestRollup,
}

#[derive(Clone, Debug, Default)]
pub struct OverviewRollup {
    pub requests: RequestRollup,
    pub attempts: BTreeMap<(String, String), AttemptRollup>,
}

#[derive(Clone, Debug)]
pub struct RequestPage {
    pub requests: Vec<RequestRecord>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum StatsPhase {
    Requests,
    Attempts,
    Ready,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StatsMeta {
    version: u32,
    phase: StatsPhase,
    request_cursor: Option<String>,
    attempt_cursor: Option<String>,
    migrated_requests: u64,
    migrated_attempts: u64,
}

impl Default for StatsMeta {
    fn default() -> Self {
        Self {
            version: STATS_SCHEMA_VERSION,
            phase: StatsPhase::Requests,
            request_cursor: None,
            attempt_cursor: None,
            migrated_requests: 0,
            migrated_attempts: 0,
        }
    }
}

#[derive(Clone)]
pub struct Store {
    database: Arc<Database>,
}

impl Store {
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(data_dir).map_err(|error| error.to_string())?;
        let database =
            Database::create(data_dir.join("quotamux.redb")).map_err(|error| error.to_string())?;
        let store = Self {
            database: Arc::new(database),
        };
        let transaction = store
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        transaction
            .open_table(STATE)
            .map_err(|error| error.to_string())?;
        transaction
            .open_table(REQUESTS)
            .map_err(|error| error.to_string())?;
        transaction
            .open_table(ATTEMPTS)
            .map_err(|error| error.to_string())?;
        transaction
            .open_table(ALERTS)
            .map_err(|error| error.to_string())?;
        open_derived_table(&transaction, REQUEST_ROLLUP_BUCKET)?;
        open_derived_table(&transaction, REQUEST_ROLLUP_ALL)?;
        open_derived_table(&transaction, ATTEMPT_ROLLUP_ALL)?;
        open_derived_table(&transaction, ATTEMPT_ROLLUP_BUCKET)?;
        transaction
            .open_table(STATS_META)
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(store)
    }

    pub fn ensure_stats_schema(&self) -> Result<(), String> {
        let mut meta = self.stats_meta()?.unwrap_or_default();
        if meta.version > STATS_SCHEMA_VERSION {
            return Err(format!(
                "unsupported statistics schema version {}; expected at most {STATS_SCHEMA_VERSION}",
                meta.version
            ));
        }
        if meta.version < STATS_SCHEMA_VERSION {
            self.reset_rollups(STATS_SCHEMA_VERSION)?;
            meta = self.stats_meta()?.unwrap_or_default();
        }
        loop {
            match meta.phase {
                StatsPhase::Requests => self.backfill_requests_batch(&mut meta)?,
                StatsPhase::Attempts => self.backfill_attempts_batch(&mut meta)?,
                StatsPhase::Ready => break,
            }
        }
        self.ensure_attempt_buckets()?;
        self.prune_expired_stats()
    }

    /// Prepares this data directory for a binary that expects an older statistics
    /// schema: only the derived rollup tables are dropped and the schema marker is
    /// rewritten, so that binary rebuilds its own rollups from the untouched ledger.
    pub fn downgrade_stats_schema(&self, version: u32) -> Result<(), String> {
        if version >= STATS_SCHEMA_VERSION {
            return Err(format!(
                "downgrade target {version} is not older than the current statistics schema {STATS_SCHEMA_VERSION}"
            ));
        }
        self.reset_rollups(version)
    }

    /// Retained ledger sizes, so a schema change can prove it never dropped records.
    pub fn ledger_counts(&self) -> Result<(u64, u64), String> {
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let requests = transaction
            .open_table(REQUESTS)
            .map_err(|error| error.to_string())?
            .len()
            .map_err(|error| error.to_string())?;
        let attempts = transaction
            .open_table(ATTEMPTS)
            .map_err(|error| error.to_string())?
            .len()
            .map_err(|error| error.to_string())?;
        Ok((requests, attempts))
    }

    /// Drops every derived statistics table and restarts the resumable backfill at
    /// `version`. The request and attempt ledgers are untouched, so all-time totals
    /// are rebuilt exactly and buckets are recreated for whatever the ledger still
    /// retains.
    fn reset_rollups(&self, version: u32) -> Result<(), String> {
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        let dropped = [
            transaction
                .delete_table(REQUEST_ROLLUP_BUCKET)
                .map_err(|error| error.to_string())?,
            transaction
                .delete_table(REQUEST_ROLLUP_ALL)
                .map_err(|error| error.to_string())?,
            transaction
                .delete_table(ATTEMPT_ROLLUP_ALL)
                .map_err(|error| error.to_string())?,
            transaction
                .delete_table(ATTEMPT_ROLLUP_BUCKET)
                .map_err(|error| error.to_string())?,
        ];
        debug_assert_eq!(dropped.len(), 4);
        {
            let mut table = transaction
                .open_table(STATS_META)
                .map_err(|error| error.to_string())?;
            table
                .remove(ATTEMPT_BUCKET_META_KEY)
                .map_err(|error| error.to_string())?;
        }
        write_stats_meta(
            &transaction,
            &StatsMeta {
                version,
                ..StatsMeta::default()
            },
        )?;
        transaction.commit().map_err(|error| error.to_string())
    }

    pub fn put_state<T: Serialize>(&self, key: &str, value: &T) -> Result<(), String> {
        self.put(STATE, key, value)
    }

    pub fn get_state<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, String> {
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let table = transaction
            .open_table(STATE)
            .map_err(|error| error.to_string())?;
        let Some(value) = table.get(key).map_err(|error| error.to_string())? else {
            return Ok(None);
        };
        serde_json::from_slice(value.value())
            .map(Some)
            .map_err(|error| error.to_string())
    }

    pub fn migrate_state_key(&self, old: &str, new: &str) -> Result<bool, String> {
        if old == new {
            return Ok(false);
        }
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        let changed = {
            let mut table = transaction
                .open_table(STATE)
                .map_err(|error| error.to_string())?;
            let old_value = table
                .get(old)
                .map_err(|error| error.to_string())?
                .map(|value| value.value().to_vec());
            if let Some(old_value) = old_value {
                if table.get(new).map_err(|error| error.to_string())?.is_none() {
                    table
                        .insert(new, old_value.as_slice())
                        .map_err(|error| error.to_string())?;
                }
                table.remove(old).map_err(|error| error.to_string())?;
                true
            } else {
                false
            }
        };
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(changed)
    }

    pub fn record_request(&self, record: &RequestRecord) -> Result<(), String> {
        let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        require_stats_ready(&transaction)?;
        {
            let mut requests = transaction
                .open_table(REQUESTS)
                .map_err(|error| error.to_string())?;
            if let Some(existing) = requests
                .get(record.id.as_str())
                .map_err(|error| error.to_string())?
            {
                if existing.value() == bytes.as_slice() {
                    return Ok(());
                }
                return Err(format!(
                    "request {} already exists with different contents",
                    record.id
                ));
            }
            requests
                .insert(record.id.as_str(), bytes.as_slice())
                .map_err(|error| error.to_string())?;
        }
        apply_request_rollup(&transaction, record, Utc::now().timestamp_millis())?;
        transaction.commit().map_err(|error| error.to_string())
    }

    pub fn record_attempt(&self, record: &AttemptRecord) -> Result<(), String> {
        let bytes = serde_json::to_vec(record).map_err(|error| error.to_string())?;
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        require_stats_ready(&transaction)?;
        {
            let mut attempts = transaction
                .open_table(ATTEMPTS)
                .map_err(|error| error.to_string())?;
            if let Some(existing) = attempts
                .get(record.id.as_str())
                .map_err(|error| error.to_string())?
            {
                if existing.value() == bytes.as_slice() {
                    return Ok(());
                }
                return Err(format!(
                    "attempt {} already exists with different contents",
                    record.id
                ));
            }
            attempts
                .insert(record.id.as_str(), bytes.as_slice())
                .map_err(|error| error.to_string())?;
        }
        apply_attempt_rollup(&transaction, record)?;
        apply_attempt_buckets(&transaction, record, Utc::now().timestamp_millis())?;
        transaction.commit().map_err(|error| error.to_string())
    }

    pub fn record_alert(&self, record: &AlertRecord) -> Result<(), String> {
        self.put(ALERTS, &record.id, record)
    }

    pub fn rename_backend(&self, old: &str, new: &str) -> Result<usize, String> {
        if old == new {
            return Ok(0);
        }
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        let mut changed = 0;
        changed += rewrite_backend_field(&transaction, REQUESTS, old, new)?;
        changed += rewrite_backend_field(&transaction, ATTEMPTS, old, new)?;
        changed += rewrite_backend_field(&transaction, ALERTS, old, new)?;
        changed += rename_circuit_state_keys(&transaction, old, new)?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(changed)
    }

    pub fn requests(&self, limit: usize) -> Result<Vec<RequestRecord>, String> {
        self.list(REQUESTS, limit)
    }

    pub fn request_page(&self, limit: usize, before: Option<&str>) -> Result<RequestPage, String> {
        let (requests, next_cursor) = self.list_page(REQUESTS, limit, before)?;
        Ok(RequestPage {
            requests,
            next_cursor,
        })
    }

    pub fn request_page_for_model(
        &self,
        limit: usize,
        before: Option<&str>,
        model_names: &[&str],
    ) -> Result<RequestPage, String> {
        let mut page = RequestPage {
            requests: Vec::new(),
            next_cursor: None,
        };
        if limit == 0 || model_names.is_empty() {
            return Ok(page);
        }
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let table = transaction
            .open_table(REQUESTS)
            .map_err(|error| error.to_string())?;
        let entries = if let Some(before) = before {
            table
                .range::<&str>((Unbounded, Excluded(before)))
                .map_err(|error| error.to_string())?
        } else {
            table.iter().map_err(|error| error.to_string())?
        };
        let mut last_key = None;
        for entry in entries.rev() {
            let (key, value) = entry.map_err(|error| error.to_string())?;
            let record: RequestRecord =
                serde_json::from_slice(value.value()).map_err(|error| error.to_string())?;
            let model = record
                .served_model
                .as_deref()
                .unwrap_or(&record.requested_model);
            if !model_names.contains(&model) {
                continue;
            }
            if page.requests.len() == limit {
                page.next_cursor = last_key;
                break;
            }
            last_key = Some(key.value().to_owned());
            page.requests.push(record);
        }
        Ok(page)
    }

    pub fn attempts(&self, limit: usize) -> Result<Vec<AttemptRecord>, String> {
        self.list(ATTEMPTS, limit)
    }

    /// Aggregates failed attempts in a window by their upstream error
    /// type/code pair.
    ///
    /// Attempt keys are UUIDv7 values, so reverse iteration visits newest first
    /// and can stop as soon as a record predates the window. Rows without any
    /// retained upstream discriminator are grouped under their failure class so
    /// transport and stream failures stay visible next to provider rejections.
    pub fn upstream_errors(
        &self,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        served_model: Option<&str>,
        limit: usize,
    ) -> Result<Vec<UpstreamErrorBucket>, String> {
        const MAX_SCAN: usize = 200_000;
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let table = transaction
            .open_table(ATTEMPTS)
            .map_err(|error| error.to_string())?;
        type BucketKey = (Option<String>, Option<String>, Option<String>);
        let mut buckets: BTreeMap<BucketKey, UpstreamErrorBucket> = BTreeMap::new();
        let mut scanned = 0usize;
        let mut stale_streak = 0usize;
        for entry in table.iter().map_err(|error| error.to_string())?.rev() {
            let (_, value) = entry.map_err(|error| error.to_string())?;
            let record: AttemptRecord = serde_json::from_slice(value.value())
                .map_err(|error| format!("attempt record is not valid JSON: {error}"))?;
            if let Some(from) = from_ms
                && record.completed_at_ms < from
            {
                // Live keys are UUIDv7 values written after completion, so the
                // reverse scan normally meets the window boundary immediately.
                // A short tail guards against any row whose key is not
                // time-ordered without turning this into a full table scan.
                stale_streak += 1;
                if stale_streak > 64 {
                    break;
                }
                continue;
            }
            stale_streak = 0;
            scanned += 1;
            if scanned > MAX_SCAN {
                break;
            }
            if let Some(to) = to_ms
                && record.completed_at_ms >= to
            {
                continue;
            }
            if let Some(model) = served_model {
                let matches = match record.served_model.as_deref() {
                    Some(served) => served == model,
                    None => record.upstream_model == model,
                };
                if !matches {
                    continue;
                }
            }
            let failed = record.error_class.is_some()
                || record.status.is_some_and(|status| status >= 400)
                || record.upstream_error_type.is_some()
                || record.upstream_error_code.is_some();
            if !failed {
                continue;
            }
            let key = (
                record.upstream_error_type.clone(),
                record.upstream_error_code.clone(),
                record.error_class.map(|class| class.as_str().to_string()),
            );
            let bucket = buckets.entry(key).or_insert_with(|| UpstreamErrorBucket {
                upstream_type: record.upstream_error_type.clone(),
                upstream_code: record.upstream_error_code.clone(),
                error_class: record.error_class,
                message: None,
                backend: Some(record.backend.clone()),
                count: 0,
                last_seen_ms: record.completed_at_ms,
            });
            bucket.count += 1;
            if record.completed_at_ms >= bucket.last_seen_ms {
                bucket.last_seen_ms = record.completed_at_ms;
                bucket.backend = Some(record.backend.clone());
                if let Some(message) = record
                    .upstream_error_message
                    .clone()
                    .or_else(|| record.sanitized_error.clone())
                {
                    bucket.message = Some(message);
                }
            } else if bucket.message.is_none() {
                bucket.message = record
                    .upstream_error_message
                    .clone()
                    .or_else(|| record.sanitized_error.clone());
            }
        }
        let mut errors = buckets.into_values().collect::<Vec<_>>();
        errors.sort_by(|a, b| {
            b.count
                .cmp(&a.count)
                .then_with(|| b.last_seen_ms.cmp(&a.last_seen_ms))
        });
        errors.truncate(limit.max(1));
        Ok(errors)
    }

    pub fn alerts(&self, limit: usize) -> Result<Vec<AlertRecord>, String> {
        self.list(ALERTS, limit)
    }

    pub fn routing_rollup(
        &self,
        served_model: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<RoutingRollup, String> {
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let mut result = RoutingRollup::default();
        match (from_ms, to_ms) {
            (None, None) => {
                let table = transaction
                    .open_table(REQUEST_ROLLUP_ALL)
                    .map_err(|error| error.to_string())?;
                let upper_model = format!("{served_model}\0");
                let start = (served_model, 0, 0, "", "", "", "");
                let end = (upper_model.as_str(), 0, 0, "", "", "", "");
                for entry in table.range(start..end).map_err(|error| error.to_string())? {
                    let (key, value) = entry.map_err(|error| error.to_string())?;
                    accumulate_request_row(&mut result, key.value(), value.value())?;
                }
            }
            (Some(from_ms), Some(to_ms)) if from_ms <= to_ms => {
                let table = transaction
                    .open_table(REQUEST_ROLLUP_BUCKET)
                    .map_err(|error| error.to_string())?;
                for (grain, start, end) in bucket_ranges(from_ms, to_ms) {
                    add_bucket_range(&table, served_model, grain, start, end, &mut result)?;
                }
            }
            _ => return Err("statistics range must provide a valid from_ms and to_ms".into()),
        }
        Ok(result)
    }

    pub fn overview_rollup(&self) -> Result<OverviewRollup, String> {
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let mut result = OverviewRollup::default();
        {
            let table = transaction
                .open_table(REQUEST_ROLLUP_ALL)
                .map_err(|error| error.to_string())?;
            for entry in table.iter().map_err(|error| error.to_string())? {
                let (key, value) = entry.map_err(|error| error.to_string())?;
                if key.value().1 == SCOPE_MODEL {
                    result
                        .requests
                        .checked_add_assign(RequestRollup::from(value.value()))?;
                }
            }
        }
        {
            let table = transaction
                .open_table(ATTEMPT_ROLLUP_ALL)
                .map_err(|error| error.to_string())?;
            for entry in table.iter().map_err(|error| error.to_string())? {
                let (key, value) = entry.map_err(|error| error.to_string())?;
                let (backend, upstream_model) = key.value();
                result.attempts.insert(
                    (backend.to_string(), upstream_model.to_string()),
                    AttemptRollup::from(value.value()),
                );
            }
        }
        Ok(result)
    }

    /// Bounded overview uses the same completed-minute ranges as model statistics.
    pub fn overview_rollup_window(
        &self,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
    ) -> Result<OverviewRollup, String> {
        let (from_ms, to_ms) = match (from_ms, to_ms) {
            (None, None) => return self.overview_rollup(),
            (Some(from), Some(to)) if from <= to => (from, to),
            _ => return Err("statistics range must provide a valid from_ms and to_ms".into()),
        };
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let all = transaction
            .open_table(REQUEST_ROLLUP_ALL)
            .map_err(|error| error.to_string())?;
        let requests = transaction
            .open_table(REQUEST_ROLLUP_BUCKET)
            .map_err(|error| error.to_string())?;
        let attempts = transaction
            .open_table(ATTEMPT_ROLLUP_BUCKET)
            .map_err(|error| error.to_string())?;
        let mut result = OverviewRollup::default();
        for entry in all.iter().map_err(|error| error.to_string())? {
            let (key, _) = entry.map_err(|error| error.to_string())?;
            let (model, scope, ..) = key.value();
            if scope != SCOPE_MODEL {
                continue;
            }
            let mut model_result = RoutingRollup::default();
            for (grain, start, end) in bucket_ranges(from_ms, to_ms) {
                add_bucket_range(&requests, model, grain, start, end, &mut model_result)?;
            }
            result.requests.checked_add_assign(model_result.total)?;
        }
        for (grain, start, end) in bucket_ranges(from_ms, to_ms) {
            if start >= end {
                continue;
            }
            for entry in attempts
                .range((grain, start, "", "")..(grain, end, "", ""))
                .map_err(|error| error.to_string())?
            {
                let (key, value) = entry.map_err(|error| error.to_string())?;
                let (_, _, backend, model) = key.value();
                result
                    .attempts
                    .entry((backend.to_owned(), model.to_owned()))
                    .or_default()
                    .checked_add_assign(AttemptRollup::from(value.value()))?;
            }
        }
        Ok(result)
    }

    fn ensure_attempt_buckets(&self) -> Result<(), String> {
        let mut meta = {
            let transaction = self
                .database
                .begin_read()
                .map_err(|error| error.to_string())?;
            let table = transaction
                .open_table(STATS_META)
                .map_err(|error| error.to_string())?;
            table
                .get(ATTEMPT_BUCKET_META_KEY)
                .map_err(|error| error.to_string())?
                .map(|value| {
                    serde_json::from_slice::<AttemptBucketMeta>(value.value())
                        .map_err(|error| error.to_string())
                })
                .transpose()?
                .unwrap_or_default()
        };
        while !meta.ready {
            self.backfill_attempt_buckets_batch(&mut meta)?;
        }
        Ok(())
    }

    fn backfill_attempt_buckets_batch(&self, meta: &mut AttemptBucketMeta) -> Result<(), String> {
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        let rows = {
            let table = transaction
                .open_table(ATTEMPTS)
                .map_err(|error| error.to_string())?;
            read_batch::<AttemptRecord>(&table, meta.cursor.as_deref())?
        };
        let now = Utc::now().timestamp_millis();
        for (_, record) in &rows {
            apply_attempt_buckets(&transaction, record, now)?;
        }
        if let Some((key, _)) = rows.last() {
            meta.cursor = Some(key.clone());
        }
        meta.ready = rows.len() < MIGRATION_BATCH_SIZE;
        {
            let bytes = serde_json::to_vec(meta).map_err(|error| error.to_string())?;
            let mut table = transaction
                .open_table(STATS_META)
                .map_err(|error| error.to_string())?;
            table
                .insert(ATTEMPT_BUCKET_META_KEY, bytes.as_slice())
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    pub fn prune_stats(&self, cutoff_ms: i64) -> Result<(), String> {
        let cutoff = floor_to(cutoff_ms, MINUTE_MS);
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        {
            let mut table = transaction
                .open_table(REQUEST_ROLLUP_BUCKET)
                .map_err(|error| error.to_string())?;
            table
                .retain(|key, _| key.2 >= cutoff)
                .map_err(|error| error.to_string())?;
        }
        {
            let mut table = transaction
                .open_table(ATTEMPT_ROLLUP_BUCKET)
                .map_err(|error| error.to_string())?;
            table
                .retain(|key, _| key.1 >= cutoff)
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    pub fn prune_expired_stats(&self) -> Result<(), String> {
        self.prune_stats(
            Utc::now()
                .timestamp_millis()
                .saturating_sub(ROLLUP_RETENTION_MS),
        )
    }

    fn stats_meta(&self) -> Result<Option<StatsMeta>, String> {
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let table = transaction
            .open_table(STATS_META)
            .map_err(|error| error.to_string())?;
        let Some(value) = table
            .get(STATS_META_KEY)
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        serde_json::from_slice(value.value()).map_err(|error| error.to_string())
    }

    fn backfill_requests_batch(&self, meta: &mut StatsMeta) -> Result<(), String> {
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        let rows = {
            let table = transaction
                .open_table(REQUESTS)
                .map_err(|error| error.to_string())?;
            read_batch::<RequestRecord>(&table, meta.request_cursor.as_deref())?
        };
        let now = Utc::now().timestamp_millis();
        for (_, record) in &rows {
            apply_request_rollup(&transaction, record, now)?;
        }
        if let Some((last_key, _)) = rows.last() {
            meta.request_cursor = Some(last_key.clone());
            meta.migrated_requests = meta
                .migrated_requests
                .checked_add(rows.len() as u64)
                .ok_or_else(|| "request migration counter overflow".to_string())?;
        }
        if rows.len() < MIGRATION_BATCH_SIZE {
            meta.phase = StatsPhase::Attempts;
        }
        write_stats_meta(&transaction, meta)?;
        transaction.commit().map_err(|error| error.to_string())
    }

    fn backfill_attempts_batch(&self, meta: &mut StatsMeta) -> Result<(), String> {
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        let rows = {
            let table = transaction
                .open_table(ATTEMPTS)
                .map_err(|error| error.to_string())?;
            read_batch::<AttemptRecord>(&table, meta.attempt_cursor.as_deref())?
        };
        for (_, record) in &rows {
            apply_attempt_rollup(&transaction, record)?;
        }
        if let Some((last_key, _)) = rows.last() {
            meta.attempt_cursor = Some(last_key.clone());
            meta.migrated_attempts = meta
                .migrated_attempts
                .checked_add(rows.len() as u64)
                .ok_or_else(|| "attempt migration counter overflow".to_string())?;
        }
        if rows.len() < MIGRATION_BATCH_SIZE {
            meta.phase = StatsPhase::Ready;
        }
        write_stats_meta(&transaction, meta)?;
        transaction.commit().map_err(|error| error.to_string())
    }

    fn put<T: Serialize>(
        &self,
        definition: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &T,
    ) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
        let transaction = self
            .database
            .begin_write()
            .map_err(|error| error.to_string())?;
        {
            let mut table = transaction
                .open_table(definition)
                .map_err(|error| error.to_string())?;
            table
                .insert(key, bytes.as_slice())
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    fn list<T: DeserializeOwned>(
        &self,
        definition: TableDefinition<&str, &[u8]>,
        limit: usize,
    ) -> Result<Vec<T>, String> {
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let table = transaction
            .open_table(definition)
            .map_err(|error| error.to_string())?;
        let mut items = Vec::with_capacity(limit);
        let entries = table.iter().map_err(|error| error.to_string())?;
        for entry in entries.rev().take(limit) {
            let (_, value) = entry.map_err(|error| error.to_string())?;
            let item = serde_json::from_slice(value.value()).map_err(|error| error.to_string())?;
            items.push(item);
        }
        Ok(items)
    }

    fn list_page<T: DeserializeOwned>(
        &self,
        definition: TableDefinition<&str, &[u8]>,
        limit: usize,
        before: Option<&str>,
    ) -> Result<(Vec<T>, Option<String>), String> {
        if limit == 0 {
            return Ok((Vec::new(), None));
        }
        let transaction = self
            .database
            .begin_read()
            .map_err(|error| error.to_string())?;
        let table = transaction
            .open_table(definition)
            .map_err(|error| error.to_string())?;
        let entries = if let Some(before) = before {
            table
                .range::<&str>((Unbounded, Excluded(before)))
                .map_err(|error| error.to_string())?
        } else {
            table.iter().map_err(|error| error.to_string())?
        };
        let mut items = Vec::with_capacity(limit);
        let mut last_key = None;
        let mut has_more = false;
        for (index, entry) in entries.rev().take(limit.saturating_add(1)).enumerate() {
            let (key, value) = entry.map_err(|error| error.to_string())?;
            if index == limit {
                has_more = true;
                break;
            }
            last_key = Some(key.value().to_string());
            items.push(serde_json::from_slice(value.value()).map_err(|error| error.to_string())?);
        }
        Ok((items, has_more.then_some(last_key).flatten()))
    }
}

impl From<RequestRollupValue> for RequestRollup {
    fn from(value: RequestRollupValue) -> Self {
        Self {
            calls: value.0,
            errors: value.1,
            fallbacks: value.2,
            input_tokens: value.3,
            cache_hit_tokens: value.4,
            cache_miss_tokens: value.5,
            reasoning_tokens: value.6,
            reasoning_estimated_tokens: value.7,
            output_tokens: value.8,
            total_tokens: value.9,
            bytes: value.10,
        }
    }
}

impl From<RequestRollup> for RequestRollupValue {
    fn from(value: RequestRollup) -> Self {
        (
            value.calls,
            value.errors,
            value.fallbacks,
            value.input_tokens,
            value.cache_hit_tokens,
            value.cache_miss_tokens,
            value.reasoning_tokens,
            value.reasoning_estimated_tokens,
            value.output_tokens,
            value.total_tokens,
            value.bytes,
        )
    }
}

impl RequestRollup {
    pub fn checked_add_assign(&mut self, other: Self) -> Result<(), String> {
        self.calls = checked_add(self.calls, other.calls, "calls")?;
        self.errors = checked_add(self.errors, other.errors, "errors")?;
        self.fallbacks = checked_add(self.fallbacks, other.fallbacks, "fallbacks")?;
        self.input_tokens = checked_add(self.input_tokens, other.input_tokens, "input tokens")?;
        self.cache_hit_tokens = checked_add(
            self.cache_hit_tokens,
            other.cache_hit_tokens,
            "cache-hit tokens",
        )?;
        self.cache_miss_tokens = checked_add(
            self.cache_miss_tokens,
            other.cache_miss_tokens,
            "cache-miss tokens",
        )?;
        self.reasoning_tokens = checked_add(
            self.reasoning_tokens,
            other.reasoning_tokens,
            "reasoning tokens",
        )?;
        self.reasoning_estimated_tokens = checked_add(
            self.reasoning_estimated_tokens,
            other.reasoning_estimated_tokens,
            "estimated reasoning tokens",
        )?;
        self.output_tokens = checked_add(self.output_tokens, other.output_tokens, "output tokens")?;
        self.total_tokens = checked_add(self.total_tokens, other.total_tokens, "total tokens")?;
        self.bytes = checked_add(self.bytes, other.bytes, "bytes")?;
        Ok(())
    }
}

impl From<AttemptRollupValue> for AttemptRollup {
    fn from(value: AttemptRollupValue) -> Self {
        Self {
            attempts: value.0,
            successes: value.1,
            errors: value.2,
            cache_reported_attempts: value.3,
            input_tokens: value.4,
            cache_hit_tokens: value.5,
            cache_miss_tokens: value.6,
            reasoning_tokens: value.7,
            reasoning_estimated_tokens: value.8,
            output_tokens: value.9,
            cost_usd: value.10,
        }
    }
}

impl From<AttemptRollup> for AttemptRollupValue {
    fn from(value: AttemptRollup) -> Self {
        (
            value.attempts,
            value.successes,
            value.errors,
            value.cache_reported_attempts,
            value.input_tokens,
            value.cache_hit_tokens,
            value.cache_miss_tokens,
            value.reasoning_tokens,
            value.reasoning_estimated_tokens,
            value.output_tokens,
            value.cost_usd,
        )
    }
}

impl AttemptRollup {
    pub fn checked_add_assign(&mut self, other: Self) -> Result<(), String> {
        self.attempts = checked_add(self.attempts, other.attempts, "attempts")?;
        self.successes = checked_add(self.successes, other.successes, "attempt successes")?;
        self.errors = checked_add(self.errors, other.errors, "attempt errors")?;
        self.cache_reported_attempts = checked_add(
            self.cache_reported_attempts,
            other.cache_reported_attempts,
            "cache-reported attempts",
        )?;
        self.input_tokens = checked_add(self.input_tokens, other.input_tokens, "input tokens")?;
        self.cache_hit_tokens = checked_add(
            self.cache_hit_tokens,
            other.cache_hit_tokens,
            "cache-hit tokens",
        )?;
        self.cache_miss_tokens = checked_add(
            self.cache_miss_tokens,
            other.cache_miss_tokens,
            "cache-miss tokens",
        )?;
        self.reasoning_tokens = checked_add(
            self.reasoning_tokens,
            other.reasoning_tokens,
            "reasoning tokens",
        )?;
        self.reasoning_estimated_tokens = checked_add(
            self.reasoning_estimated_tokens,
            other.reasoning_estimated_tokens,
            "estimated reasoning tokens",
        )?;
        self.output_tokens = checked_add(self.output_tokens, other.output_tokens, "output tokens")?;
        self.cost_usd += other.cost_usd;
        if !self.cost_usd.is_finite() {
            return Err("provider cost overflow".into());
        }
        Ok(())
    }
}

fn checked_add(left: u64, right: u64, field: &str) -> Result<u64, String> {
    left.checked_add(right)
        .ok_or_else(|| format!("statistics {field} overflow"))
}

fn read_batch<T: DeserializeOwned>(
    table: &redb::Table<'_, &str, &[u8]>,
    cursor: Option<&str>,
) -> Result<Vec<(String, T)>, String> {
    let mut rows = Vec::with_capacity(MIGRATION_BATCH_SIZE);
    if let Some(cursor) = cursor {
        let entries = table
            .range::<&str>((Excluded(cursor), Unbounded))
            .map_err(|error| error.to_string())?;
        for entry in entries.take(MIGRATION_BATCH_SIZE) {
            let (key, value) = entry.map_err(|error| error.to_string())?;
            rows.push((
                key.value().to_string(),
                serde_json::from_slice(value.value()).map_err(|error| error.to_string())?,
            ));
        }
    } else {
        let entries = table.iter().map_err(|error| error.to_string())?;
        for entry in entries.take(MIGRATION_BATCH_SIZE) {
            let (key, value) = entry.map_err(|error| error.to_string())?;
            rows.push((
                key.value().to_string(),
                serde_json::from_slice(value.value()).map_err(|error| error.to_string())?,
            ));
        }
    }
    Ok(rows)
}

/// Opens a derived statistics table, replacing one left behind by an older schema
/// version whose stored key or value type no longer matches. The rebuild triggered
/// by `ensure_stats_schema` repopulates it from the retained ledger.
fn open_derived_table<K, V>(
    transaction: &WriteTransaction,
    definition: TableDefinition<K, V>,
) -> Result<(), String>
where
    K: redb::Key + 'static,
    V: redb::Value + 'static,
{
    match transaction.open_table(definition) {
        Ok(_) => Ok(()),
        Err(redb::TableError::TableTypeMismatch { .. }) => {
            let name = definition.name();
            transaction
                .delete_table(TableDefinition::<K, V>::new(name))
                .map_err(|error| error.to_string())?;
            transaction
                .open_table(TableDefinition::<K, V>::new(name))
                .map(|_| ())
                .map_err(|error| error.to_string())
        }
        Err(error) => Err(error.to_string()),
    }
}

fn write_stats_meta(transaction: &WriteTransaction, meta: &StatsMeta) -> Result<(), String> {
    let bytes = serde_json::to_vec(meta).map_err(|error| error.to_string())?;
    let mut table = transaction
        .open_table(STATS_META)
        .map_err(|error| error.to_string())?;
    table
        .insert(STATS_META_KEY, bytes.as_slice())
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn require_stats_ready(transaction: &WriteTransaction) -> Result<(), String> {
    let table = transaction
        .open_table(STATS_META)
        .map_err(|error| error.to_string())?;
    let Some(value) = table
        .get(STATS_META_KEY)
        .map_err(|error| error.to_string())?
    else {
        return Err("statistics schema is not initialized".into());
    };
    let meta =
        serde_json::from_slice::<StatsMeta>(value.value()).map_err(|error| error.to_string())?;
    if meta.version != STATS_SCHEMA_VERSION || meta.phase != StatsPhase::Ready {
        return Err("statistics schema migration is not complete".into());
    }
    let bucket_meta = table
        .get(ATTEMPT_BUCKET_META_KEY)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "attempt bucket migration is not initialized".to_string())?;
    let bucket_meta: AttemptBucketMeta =
        serde_json::from_slice(bucket_meta.value()).map_err(|error| error.to_string())?;
    if !bucket_meta.ready {
        return Err("attempt bucket migration is not complete".into());
    }
    Ok(())
}

fn apply_request_rollup(
    transaction: &WriteTransaction,
    record: &RequestRecord,
    now_ms: i64,
) -> Result<(), String> {
    let served_model = record.served_model.as_deref().unwrap_or(UNCLASSIFIED_MODEL);
    let delta = RequestRollup {
        calls: 1,
        errors: u64::from(record.error_class.is_some()),
        fallbacks: u64::from(record.fallback == Some(true)),
        input_tokens: record.usage.input_tokens,
        cache_hit_tokens: record.usage.cache_hit_tokens,
        cache_miss_tokens: record.usage.cache_miss_tokens,
        reasoning_tokens: record.usage.reasoning_tokens,
        reasoning_estimated_tokens: record.usage.reasoning_estimated_tokens,
        output_tokens: record.usage.output_tokens,
        total_tokens: record.usage.total_tokens,
        bytes: record
            .request_bytes
            .checked_add(record.response_bytes)
            .ok_or_else(|| "request byte count overflow".to_string())?,
    };
    let target = request_target(record);
    let cutoff = now_ms.saturating_sub(ROLLUP_RETENTION_MS);
    if record.started_at_ms >= cutoff {
        let mut table = transaction
            .open_table(REQUEST_ROLLUP_BUCKET)
            .map_err(|error| error.to_string())?;
        for (grain, bucket_ms) in [
            (GRAIN_MINUTE, floor_to(record.started_at_ms, MINUTE_MS)),
            (GRAIN_HOUR, floor_to(record.started_at_ms, HOUR_MS)),
        ] {
            increment_request_bucket(
                &mut table,
                (
                    served_model,
                    grain,
                    bucket_ms,
                    SCOPE_MODEL,
                    0,
                    "",
                    "",
                    "",
                    "",
                ),
                delta,
            )?;
            match target.as_ref() {
                Some(target) => increment_request_bucket(
                    &mut table,
                    (
                        served_model,
                        grain,
                        bucket_ms,
                        SCOPE_TARGET,
                        target.layer_index,
                        target.layer_name.as_str(),
                        target.backend.as_str(),
                        target.credential.as_str(),
                        target.upstream_model.as_str(),
                    ),
                    delta,
                )?,
                None => increment_request_bucket(
                    &mut table,
                    (
                        served_model,
                        grain,
                        bucket_ms,
                        SCOPE_UNATTRIBUTED,
                        0,
                        "",
                        "",
                        "",
                        "",
                    ),
                    delta,
                )?,
            }
        }
    }
    let mut table = transaction
        .open_table(REQUEST_ROLLUP_ALL)
        .map_err(|error| error.to_string())?;
    increment_request_all(
        &mut table,
        (served_model, SCOPE_MODEL, 0, "", "", "", ""),
        delta,
    )?;
    match target {
        Some(target) => increment_request_all(
            &mut table,
            (
                served_model,
                SCOPE_TARGET,
                target.layer_index,
                target.layer_name.as_str(),
                target.backend.as_str(),
                target.credential.as_str(),
                target.upstream_model.as_str(),
            ),
            delta,
        ),
        None => increment_request_all(
            &mut table,
            (served_model, SCOPE_UNATTRIBUTED, 0, "", "", "", ""),
            delta,
        ),
    }
}

fn request_target(record: &RequestRecord) -> Option<RouteRollupKey> {
    Some(RouteRollupKey {
        layer_index: u32::try_from(record.route_layer_index?).ok()?,
        layer_name: record.route_layer.clone()?,
        backend: record.backend.clone()?,
        credential: record.credential.clone()?,
        upstream_model: record.upstream_model.clone()?,
    })
}

fn increment_request_bucket(
    table: &mut redb::Table<'_, RequestBucketKey<'static>, RequestRollupValue>,
    key: RequestBucketKey<'_>,
    delta: RequestRollup,
) -> Result<(), String> {
    let mut value = table
        .get(key)
        .map_err(|error| error.to_string())?
        .map(|value| RequestRollup::from(value.value()))
        .unwrap_or_default();
    value.checked_add_assign(delta)?;
    table
        .insert(key, RequestRollupValue::from(value))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn increment_request_all(
    table: &mut redb::Table<'_, RequestAllKey<'static>, RequestRollupValue>,
    key: RequestAllKey<'_>,
    delta: RequestRollup,
) -> Result<(), String> {
    let mut value = table
        .get(key)
        .map_err(|error| error.to_string())?
        .map(|value| RequestRollup::from(value.value()))
        .unwrap_or_default();
    value.checked_add_assign(delta)?;
    table
        .insert(key, RequestRollupValue::from(value))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn apply_attempt_rollup(
    transaction: &WriteTransaction,
    record: &AttemptRecord,
) -> Result<(), String> {
    let delta = attempt_delta(record)?;
    let key = (record.backend.as_str(), record.upstream_model.as_str());
    let mut table = transaction
        .open_table(ATTEMPT_ROLLUP_ALL)
        .map_err(|error| error.to_string())?;
    let mut value = table
        .get(key)
        .map_err(|error| error.to_string())?
        .map(|value| AttemptRollup::from(value.value()))
        .unwrap_or_default();
    value.checked_add_assign(delta)?;
    table
        .insert(key, AttemptRollupValue::from(value))
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn attempt_delta(record: &AttemptRecord) -> Result<AttemptRollup, String> {
    let delta = AttemptRollup {
        attempts: 1,
        successes: u64::from(record.error_class.is_none()),
        errors: u64::from(record.error_class.is_some()),
        cache_reported_attempts: u64::from(record.usage.provider_reported),
        input_tokens: record.usage.input_tokens,
        cache_hit_tokens: record.usage.cache_hit_tokens,
        cache_miss_tokens: record.usage.cache_miss_tokens,
        reasoning_tokens: record.usage.reasoning_tokens,
        reasoning_estimated_tokens: record.usage.reasoning_estimated_tokens,
        output_tokens: record.usage.output_tokens,
        cost_usd: record.provider_cost_usd.unwrap_or(0.0),
    };
    if !delta.cost_usd.is_finite() {
        return Err("provider cost is not finite".into());
    }
    Ok(delta)
}

fn apply_attempt_buckets(
    transaction: &WriteTransaction,
    record: &AttemptRecord,
    now_ms: i64,
) -> Result<(), String> {
    if record.started_at_ms < now_ms.saturating_sub(ROLLUP_RETENTION_MS) {
        return Ok(());
    }
    let delta = attempt_delta(record)?;
    let mut table = transaction
        .open_table(ATTEMPT_ROLLUP_BUCKET)
        .map_err(|error| error.to_string())?;
    for (grain, unit) in [(GRAIN_MINUTE, MINUTE_MS), (GRAIN_HOUR, HOUR_MS)] {
        let key = (
            grain,
            floor_to(record.started_at_ms, unit),
            record.backend.as_str(),
            record.upstream_model.as_str(),
        );
        let mut value = table
            .get(key)
            .map_err(|error| error.to_string())?
            .map(|value| AttemptRollup::from(value.value()))
            .unwrap_or_default();
        value.checked_add_assign(delta)?;
        table
            .insert(key, AttemptRollupValue::from(value))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn bucket_ranges(from_ms: i64, to_ms: i64) -> [(u8, i64, i64); 3] {
    let first_full_hour = ceil_to(from_ms, HOUR_MS);
    let last_full_hour = floor_to(to_ms, HOUR_MS);
    let left_end = first_full_hour.min(to_ms);
    [
        (GRAIN_MINUTE, from_ms, left_end),
        (GRAIN_HOUR, first_full_hour, last_full_hour),
        (GRAIN_MINUTE, last_full_hour.max(left_end), to_ms),
    ]
}

fn add_bucket_range(
    table: &redb::ReadOnlyTable<RequestBucketKey<'static>, RequestRollupValue>,
    served_model: &str,
    grain: u8,
    start_ms: i64,
    end_ms: i64,
    result: &mut RoutingRollup,
) -> Result<(), String> {
    if start_ms >= end_ms {
        return Ok(());
    }
    let start = (served_model, grain, start_ms, 0, 0, "", "", "", "");
    let end = (served_model, grain, end_ms, 0, 0, "", "", "", "");
    for entry in table.range(start..end).map_err(|error| error.to_string())? {
        let (key, value) = entry.map_err(|error| error.to_string())?;
        accumulate_request_bucket_row(result, key.value(), value.value())?;
    }
    Ok(())
}

fn accumulate_request_row(
    result: &mut RoutingRollup,
    key: RequestAllKey<'_>,
    value: RequestRollupValue,
) -> Result<(), String> {
    let (_, scope, layer_index, layer_name, backend, credential, upstream_model) = key;
    let value = RequestRollup::from(value);
    match scope {
        SCOPE_MODEL => result.total.checked_add_assign(value),
        SCOPE_TARGET => result
            .targets
            .entry(RouteRollupKey {
                layer_index,
                layer_name: layer_name.to_string(),
                backend: backend.to_string(),
                credential: credential.to_string(),
                upstream_model: upstream_model.to_string(),
            })
            .or_default()
            .checked_add_assign(value),
        SCOPE_UNATTRIBUTED => result.unattributed.checked_add_assign(value),
        _ => Err(format!("unknown request rollup scope {scope}")),
    }
}

fn accumulate_request_bucket_row(
    result: &mut RoutingRollup,
    key: RequestBucketKey<'_>,
    value: RequestRollupValue,
) -> Result<(), String> {
    let (model, _, _, scope, layer_index, layer_name, backend, credential, upstream_model) = key;
    accumulate_request_row(
        result,
        (
            model,
            scope,
            layer_index,
            layer_name,
            backend,
            credential,
            upstream_model,
        ),
        value,
    )
}

fn floor_to(value: i64, unit: i64) -> i64 {
    value.div_euclid(unit) * unit
}

fn ceil_to(value: i64, unit: i64) -> i64 {
    let floor = floor_to(value, unit);
    if floor == value {
        value
    } else {
        floor.saturating_add(unit)
    }
}

fn rewrite_backend_field(
    transaction: &WriteTransaction,
    definition: TableDefinition<&str, &[u8]>,
    old: &str,
    new: &str,
) -> Result<usize, String> {
    let mut table = transaction
        .open_table(definition)
        .map_err(|error| error.to_string())?;
    let mut updates = Vec::new();
    {
        let entries = table.iter().map_err(|error| error.to_string())?;
        for entry in entries {
            let (key, value) = entry.map_err(|error| error.to_string())?;
            let mut record: Value =
                serde_json::from_slice(value.value()).map_err(|error| error.to_string())?;
            let current = record
                .get("backend")
                .or_else(|| record.get("provider"))
                .and_then(Value::as_str);
            if current != Some(old) {
                continue;
            }
            if let Some(object) = record.as_object_mut() {
                object.remove("provider");
                object.insert("backend".into(), Value::String(new.to_string()));
            }
            updates.push((
                key.value().to_string(),
                serde_json::to_vec(&record).map_err(|error| error.to_string())?,
            ));
        }
    }
    for (key, value) in &updates {
        table
            .insert(key.as_str(), value.as_slice())
            .map_err(|error| error.to_string())?;
    }
    Ok(updates.len())
}

fn rename_circuit_state_keys(
    transaction: &WriteTransaction,
    old: &str,
    new: &str,
) -> Result<usize, String> {
    let mut table = transaction
        .open_table(STATE)
        .map_err(|error| error.to_string())?;
    let old_prefix = format!("circuit:{old}:");
    let new_prefix = format!("circuit:{new}:");
    let mut moves = Vec::new();
    {
        let entries = table.iter().map_err(|error| error.to_string())?;
        for entry in entries {
            let (key, value) = entry.map_err(|error| error.to_string())?;
            let key = key.value();
            let Some(suffix) = key.strip_prefix(&old_prefix) else {
                continue;
            };
            moves.push((
                key.to_string(),
                format!("{new_prefix}{suffix}"),
                value.value().to_vec(),
            ));
        }
    }
    for (old_key, new_key, value) in &moves {
        let destination_exists = table
            .get(new_key.as_str())
            .map_err(|error| error.to_string())?
            .is_some();
        if !destination_exists {
            table
                .insert(new_key.as_str(), value.as_slice())
                .map_err(|error| error.to_string())?;
        }
        table
            .remove(old_key.as_str())
            .map_err(|error| error.to_string())?;
    }
    Ok(moves.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    use crate::{
        config::AdapterKind,
        types::{FailureClass, Protocol, Usage},
    };

    #[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
    struct Example {
        value: u32,
    }

    #[test]
    fn round_trips_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.put_state("example", &Example { value: 7 }).unwrap();
        assert_eq!(
            store.get_state::<Example>("example").unwrap(),
            Some(Example { value: 7 })
        );
    }

    #[test]
    fn migrates_state_keys_without_overwriting_a_newer_destination() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.put_state("old", &Example { value: 7 }).unwrap();
        assert!(store.migrate_state_key("old", "new").unwrap());
        assert_eq!(
            store.get_state::<Example>("new").unwrap(),
            Some(Example { value: 7 })
        );
        assert!(!store.migrate_state_key("old", "new").unwrap());

        store.put_state("old", &Example { value: 8 }).unwrap();
        store.put_state("new", &Example { value: 9 }).unwrap();
        assert!(store.migrate_state_key("old", "new").unwrap());
        assert_eq!(
            store.get_state::<Example>("new").unwrap(),
            Some(Example { value: 9 })
        );
        assert!(store.get_state::<Example>("old").unwrap().is_none());
    }

    #[test]
    fn renames_provider_records_and_circuit_keys_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store
            .put(
                REQUESTS,
                "request",
                &serde_json::json!({"provider":"open-code-go"}),
            )
            .unwrap();
        store
            .put(
                ATTEMPTS,
                "attempt",
                &serde_json::json!({"provider":"open-code-go"}),
            )
            .unwrap();
        store
            .put(
                ALERTS,
                "alert",
                &serde_json::json!({"provider":"open-code-go"}),
            )
            .unwrap();
        store
            .put_state(
                "circuit:open-code-go:key:model",
                &serde_json::json!({"mode":"closed"}),
            )
            .unwrap();

        assert_eq!(
            store.rename_backend("open-code-go", "opencode-go").unwrap(),
            4
        );
        assert_eq!(
            store.rename_backend("open-code-go", "opencode-go").unwrap(),
            0
        );

        for (definition, key) in [
            (REQUESTS, "request"),
            (ATTEMPTS, "attempt"),
            (ALERTS, "alert"),
        ] {
            let transaction = store.database.begin_read().unwrap();
            let table = transaction.open_table(definition).unwrap();
            let record: Value =
                serde_json::from_slice(table.get(key).unwrap().unwrap().value()).unwrap();
            assert_eq!(record["backend"], "opencode-go");
        }
        assert!(
            store
                .get_state::<Value>("circuit:open-code-go:key:model")
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .get_state::<Value>("circuit:opencode-go:key:model")
                .unwrap()
                .unwrap()["mode"],
            "closed"
        );
    }

    #[test]
    fn rollups_keep_estimated_reasoning_tokens_separate_from_reported_ones() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let now = floor_to(Utc::now().timestamp_millis(), MINUTE_MS) - MINUTE_MS;

        let mut reported = request_record("reported-thinking", now);
        reported.usage.reasoning_tokens = 5;
        reported.usage.reasoning_reported = true;
        reported.usage.reasoning_estimated_tokens = 0;
        store.record_request(&reported).unwrap();

        let mut estimated = request_record("estimated-thinking", now + 1_000);
        estimated.usage.reasoning_tokens = 0;
        estimated.usage.reasoning_reported = false;
        estimated.usage.reasoning_estimated_tokens = 7;
        store.record_request(&estimated).unwrap();

        let mut reported_attempt = attempt_record("reported-thinking-attempt", now);
        reported_attempt.usage.reasoning_tokens = 5;
        reported_attempt.usage.reasoning_reported = true;
        reported_attempt.usage.reasoning_estimated_tokens = 0;
        store.record_attempt(&reported_attempt).unwrap();

        let mut estimated_attempt = attempt_record("estimated-thinking-attempt", now + 1_000);
        estimated_attempt.usage.reasoning_tokens = 0;
        estimated_attempt.usage.reasoning_reported = false;
        estimated_attempt.usage.reasoning_estimated_tokens = 7;
        store.record_attempt(&estimated_attempt).unwrap();

        let all = store.routing_rollup("served", None, None).unwrap();
        assert_eq!(all.total.calls, 2);
        assert_eq!(all.total.reasoning_tokens, 5);
        assert_eq!(all.total.reasoning_estimated_tokens, 7);

        let attempts = store.overview_rollup().unwrap();
        let row = attempts
            .attempts
            .values()
            .next()
            .expect("attempt rollup row");
        assert_eq!(row.reasoning_tokens, 5);
        assert_eq!(row.reasoning_estimated_tokens, 7);
    }

    #[test]
    fn records_and_queries_request_rollups_without_double_counting() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let now = floor_to(Utc::now().timestamp_millis(), MINUTE_MS) - MINUTE_MS;
        let request = request_record("request-1", now);

        store.record_request(&request).unwrap();
        store.record_request(&request).unwrap();

        let all = store.routing_rollup("served", None, None).unwrap();
        assert_eq!(all.total.calls, 1);
        assert_eq!(all.total.input_tokens, 10);
        assert_eq!(all.total.cache_hit_tokens, 4);
        assert_eq!(all.total.cache_miss_tokens, 6);
        assert_eq!(all.total.reasoning_tokens, 2);
        assert_eq!(all.total.output_tokens, 3);
        assert_eq!(all.targets.len(), 1);
        let (target, value) = all.targets.first_key_value().unwrap();
        assert_eq!(target.layer_index, 0);
        assert_eq!(target.layer_name, "plan");
        assert_eq!(target.backend, "provider");
        assert_eq!(target.credential, "key-a");
        assert_eq!(value.calls, 1);

        let window = store
            .routing_rollup("served", Some(now), Some(now + MINUTE_MS))
            .unwrap();
        assert_eq!(window.total, all.total);

        let mut changed = request.clone();
        changed.status = 500;
        assert!(store.record_request(&changed).is_err());
        assert_eq!(
            store
                .routing_rollup("served", None, None)
                .unwrap()
                .total
                .calls,
            1
        );
    }

    #[test]
    fn refuses_live_writes_until_the_stats_schema_is_ready() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let now = Utc::now().timestamp_millis();
        assert!(
            store
                .record_request(&request_record("request-before-ready", now))
                .is_err()
        );
        assert!(
            store
                .record_attempt(&attempt_record("attempt-before-ready", now))
                .is_err()
        );
        assert!(store.requests(10).unwrap().is_empty());
        assert!(store.attempts(10).unwrap().is_empty());
    }

    #[test]
    fn unattributed_requests_reconcile_with_model_total() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let mut request = request_record("request-unattributed", Utc::now().timestamp_millis());
        request.backend = None;
        request.credential = None;
        request.route_layer = None;
        request.route_layer_index = None;
        request.upstream_model = None;
        store.record_request(&request).unwrap();

        let stats = store.routing_rollup("served", None, None).unwrap();
        assert_eq!(stats.total.calls, 1);
        assert_eq!(stats.unattributed.calls, 1);
        assert!(stats.targets.is_empty());
    }

    #[test]
    fn backfills_legacy_rows_once_and_builds_overview_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let request = request_record("legacy-request", Utc::now().timestamp_millis());
        let attempt = attempt_record("legacy-attempt", Utc::now().timestamp_millis());
        store.put(REQUESTS, &request.id, &request).unwrap();
        store.put(ATTEMPTS, &attempt.id, &attempt).unwrap();

        store.ensure_stats_schema().unwrap();
        store.ensure_stats_schema().unwrap();

        let overview = store.overview_rollup().unwrap();
        assert_eq!(overview.requests.calls, 1);
        let attempt_stats = overview
            .attempts
            .get(&("provider".into(), "upstream".into()))
            .unwrap();
        assert_eq!(attempt_stats.attempts, 1);
        assert_eq!(attempt_stats.successes, 1);
        assert_eq!(attempt_stats.cache_reported_attempts, 1);
        assert_eq!(attempt_stats.cache_hit_tokens, 4);
        assert_eq!(attempt_stats.cache_miss_tokens, 6);
        assert_eq!(attempt_stats.input_tokens, 10);
        assert_eq!(attempt_stats.reasoning_tokens, 2);
        assert_eq!(attempt_stats.output_tokens, 3);
        assert_eq!(attempt_stats.cost_usd, 0.25);
    }

    #[test]
    fn upgrades_version_one_rollups_by_rebuilding_them_from_the_ledger() {
        type VersionOneValue = (u64, u64, u64, u64, u64, u64, u64);
        const VERSION_ONE_ALL: TableDefinition<RequestAllKey<'static>, VersionOneValue> =
            TableDefinition::new("request_rollup_all_v1");

        let dir = tempfile::tempdir().unwrap();
        let now = Utc::now().timestamp_millis();
        {
            let database = Database::create(dir.path().join("quotamux.redb")).unwrap();
            let transaction = database.begin_write().unwrap();
            for (definition, id, record) in [
                (
                    REQUESTS,
                    "legacy-request",
                    serde_json::to_vec(&request_record("legacy-request", now)).unwrap(),
                ),
                (
                    ATTEMPTS,
                    "legacy-attempt",
                    serde_json::to_vec(&attempt_record("legacy-attempt", now)).unwrap(),
                ),
            ] {
                let mut table = transaction.open_table(definition).unwrap();
                table.insert(id, record.as_slice()).unwrap();
            }
            {
                let mut table = transaction.open_table(VERSION_ONE_ALL).unwrap();
                table
                    .insert(
                        ("served", SCOPE_MODEL, 0, "", "", "", ""),
                        (99, 0, 0, 990, 990, 990, 0),
                    )
                    .unwrap();
            }
            {
                let mut table = transaction.open_table(STATS_META).unwrap();
                let meta = serde_json::to_vec(&StatsMeta {
                    version: 1,
                    phase: StatsPhase::Ready,
                    request_cursor: None,
                    attempt_cursor: None,
                    migrated_requests: 0,
                    migrated_attempts: 0,
                })
                .unwrap();
                table.insert(STATS_META_KEY, meta.as_slice()).unwrap();
            }
            transaction.commit().unwrap();
        }

        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        store.ensure_stats_schema().unwrap();

        let rollup = store.routing_rollup("served", None, None).unwrap();
        assert_eq!(rollup.total.calls, 1, "stale version-one rows must be gone");
        assert_eq!(rollup.total.input_tokens, 10);
        assert_eq!(rollup.total.cache_hit_tokens, 4);
        assert_eq!(rollup.total.cache_miss_tokens, 6);
        assert_eq!(rollup.total.reasoning_tokens, 2);
        assert_eq!(rollup.total.total_tokens, 13);
        let overview = store.overview_rollup().unwrap();
        let attempts = overview
            .attempts
            .get(&("provider".into(), "upstream".into()))
            .unwrap();
        assert_eq!(attempts.attempts, 1);
        assert_eq!(attempts.input_tokens, 10);
        assert_eq!(attempts.cache_hit_tokens, 4);
        assert_eq!(attempts.cache_miss_tokens, 6);
        assert_eq!(attempts.reasoning_tokens, 2);
        assert_eq!(attempts.output_tokens, 3);
        assert_eq!(store.requests(10).unwrap().len(), 1);
        assert_eq!(store.attempts(10).unwrap().len(), 1);
    }

    #[test]
    fn downgrades_the_statistics_schema_without_touching_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let now = floor_to(Utc::now().timestamp_millis(), MINUTE_MS);
        store
            .record_request(&request_record("request-1", now))
            .unwrap();
        store
            .record_attempt(&attempt_record("attempt-1", now))
            .unwrap();
        let before = store.ledger_counts().unwrap();
        assert_eq!(before, (1, 1));

        store.downgrade_stats_schema(1).unwrap();
        assert_eq!(store.stats_meta().unwrap().unwrap().version, 1);
        assert_eq!(store.ledger_counts().unwrap(), before);
        assert_eq!(
            store.requests(10).unwrap()[0].usage.cache_miss_tokens,
            6,
            "the retained ledger keeps its usage detail"
        );
        assert!(
            store.downgrade_stats_schema(STATS_SCHEMA_VERSION).is_err(),
            "only an older target version may be requested"
        );

        // The current binary rebuilds its own rollups from the same ledger.
        store.ensure_stats_schema().unwrap();
        let rollup = store.routing_rollup("served", None, None).unwrap();
        assert_eq!(rollup.total.calls, 1);
        assert_eq!(rollup.total.cache_hit_tokens, 4);
        assert_eq!(rollup.total.cache_miss_tokens, 6);
        assert_eq!(rollup.total.reasoning_tokens, 2);
        let attempts = store
            .overview_rollup()
            .unwrap()
            .attempts
            .get(&("provider".into(), "upstream".into()))
            .copied()
            .unwrap();
        assert_eq!(attempts.attempts, 1);
        assert_eq!(attempts.cache_hit_tokens, 4);
    }

    #[test]
    fn rejects_a_future_statistics_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let transaction = store.database.begin_write().unwrap();
        write_stats_meta(
            &transaction,
            &StatsMeta {
                version: STATS_SCHEMA_VERSION + 1,
                ..StatsMeta::default()
            },
        )
        .unwrap();
        transaction.commit().unwrap();
        assert!(store.ensure_stats_schema().is_err());
    }

    #[test]
    fn resumes_a_multi_batch_backfill_without_reapplying_completed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let transaction = store.database.begin_write().unwrap();
        {
            let mut table = transaction.open_table(REQUESTS).unwrap();
            for index in 0..=MIGRATION_BATCH_SIZE {
                let id = format!("legacy-{index:05}");
                let record = request_record(&id, Utc::now().timestamp_millis());
                let bytes = serde_json::to_vec(&record).unwrap();
                table.insert(id.as_str(), bytes.as_slice()).unwrap();
            }
        }
        transaction.commit().unwrap();

        let mut meta = StatsMeta::default();
        store.backfill_requests_batch(&mut meta).unwrap();
        assert_eq!(meta.phase, StatsPhase::Requests);
        assert_eq!(meta.migrated_requests, MIGRATION_BATCH_SIZE as u64);
        assert_eq!(
            store
                .routing_rollup("served", None, None)
                .unwrap()
                .total
                .calls,
            MIGRATION_BATCH_SIZE as u64
        );

        drop(store);
        let reopened = Store::open(dir.path()).unwrap();
        reopened.ensure_stats_schema().unwrap();
        reopened.ensure_stats_schema().unwrap();
        assert_eq!(
            reopened
                .routing_rollup("served", None, None)
                .unwrap()
                .total
                .calls,
            (MIGRATION_BATCH_SIZE + 1) as u64
        );
    }

    #[test]
    fn combines_partial_minute_ranges_with_full_hour_buckets_and_prunes_only_buckets() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let hour = floor_to(Utc::now().timestamp_millis(), HOUR_MS) - 4 * HOUR_MS;
        for (id, offset_minutes) in [("r0", 5), ("r1", 15), ("r2", 65), ("r3", 125)] {
            store
                .record_request(&request_record(id, hour + offset_minutes * MINUTE_MS))
                .unwrap();
        }

        let mixed = store
            .routing_rollup(
                "served",
                Some(hour + 10 * MINUTE_MS),
                Some(hour + 130 * MINUTE_MS),
            )
            .unwrap();
        assert_eq!(mixed.total.calls, 3);
        assert_eq!(mixed.total.total_tokens, 39);
        let exact_hour = store
            .routing_rollup("served", Some(hour + HOUR_MS), Some(hour + 2 * HOUR_MS))
            .unwrap();
        assert_eq!(exact_hour.total.calls, 1);

        store.prune_stats(hour + 2 * HOUR_MS).unwrap();
        let retained = store
            .routing_rollup("served", Some(hour), Some(hour + 3 * HOUR_MS))
            .unwrap();
        assert_eq!(retained.total.calls, 1);
        assert_eq!(
            store
                .routing_rollup("served", None, None)
                .unwrap()
                .total
                .calls,
            4
        );
    }

    #[test]
    fn attempt_rollups_are_idempotent_and_reject_conflicting_ids() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let attempt = attempt_record("attempt-1", Utc::now().timestamp_millis());
        store.record_attempt(&attempt).unwrap();
        store.record_attempt(&attempt).unwrap();
        let stats = store.overview_rollup().unwrap();
        assert_eq!(stats.attempts.values().next().unwrap().attempts, 1);

        let mut changed = attempt;
        changed.provider_cost_usd = Some(0.5);
        assert!(store.record_attempt(&changed).is_err());
        assert_eq!(
            store
                .overview_rollup()
                .unwrap()
                .attempts
                .values()
                .next()
                .unwrap()
                .attempts,
            1
        );
    }

    #[test]
    fn recent_records_read_newest_keys_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for id in ["0001", "0002", "0003"] {
            let request = request_record(id, Utc::now().timestamp_millis());
            store.put(REQUESTS, id, &request).unwrap();
        }
        let rows = store.requests(2).unwrap();
        assert_eq!(
            rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
            ["0003", "0002"]
        );
    }

    #[test]
    fn request_pages_use_exclusive_bounded_cursors() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for id in ["0001", "0002", "0003", "0004", "0005"] {
            let request = request_record(id, Utc::now().timestamp_millis());
            store.put(REQUESTS, id, &request).unwrap();
        }

        let first = store.request_page(2, None).unwrap();
        assert_eq!(
            first
                .requests
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["0005", "0004"]
        );
        assert_eq!(first.next_cursor.as_deref(), Some("0004"));

        let newest = request_record("0006", Utc::now().timestamp_millis());
        store.put(REQUESTS, "0006", &newest).unwrap();
        let second = store.request_page(2, first.next_cursor.as_deref()).unwrap();
        assert_eq!(
            second
                .requests
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["0003", "0002"]
        );
        assert_eq!(second.next_cursor.as_deref(), Some("0002"));

        let last = store
            .request_page(2, second.next_cursor.as_deref())
            .unwrap();
        assert_eq!(
            last.requests
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            ["0001"]
        );
        assert_eq!(last.next_cursor, None);
    }

    #[test]
    fn model_pages_scan_interleaved_rows_before_paginating() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        for (id, served, requested) in [
            ("01", None, "alias"),
            ("02", Some("other"), "alias"),
            ("03", Some("served"), "unrelated-request-name"),
            ("04", None, "other"),
            ("05", Some("other"), "served"),
            ("06", None, "served"),
            ("07", Some("other"), "other"),
            ("08", Some("other"), "other"),
            ("09", Some("served"), "alias"),
            ("10", Some("other"), "other"),
        ] {
            let mut record = request_record(id, Utc::now().timestamp_millis());
            record.served_model = served.map(str::to_owned);
            record.requested_model = requested.into();
            store.put(REQUESTS, id, &record).unwrap();
        }
        let names = ["served", "alias"];
        let ids = |page: &RequestPage| {
            page.requests
                .iter()
                .map(|r| r.id.clone())
                .collect::<Vec<_>>()
        };
        let first = store.request_page_for_model(2, None, &names).unwrap();
        assert_eq!(ids(&first), ["09", "06"]);
        assert_eq!(first.next_cursor.as_deref(), Some("06"));
        let second = store
            .request_page_for_model(2, first.next_cursor.as_deref(), &names)
            .unwrap();
        assert_eq!(ids(&second), ["03", "01"]);
        assert!(second.next_cursor.is_none());
        let last = store.request_page_for_model(2, Some("03"), &names).unwrap();
        assert_eq!(ids(&last), ["01"]);
        assert!(last.next_cursor.is_none());
        let all = store.request_page(2, None).unwrap();
        assert_eq!(ids(&all), ["10", "09"]);
        assert_eq!(all.next_cursor.as_deref(), Some("09"));
        for page in [
            store.request_page_for_model(0, None, &names).unwrap(),
            store.request_page_for_model(2, None, &[]).unwrap(),
            store.request_page_for_model(2, None, &["absent"]).unwrap(),
            store.request_page_for_model(2, Some("01"), &names).unwrap(),
        ] {
            assert!(page.requests.is_empty());
            assert!(page.next_cursor.is_none());
        }
    }

    #[test]
    fn request_records_read_boolean_fallback_without_exhaustion_marker() {
        let mut stored = serde_json::to_value(request_record(
            "legacy-fallback-shape",
            Utc::now().timestamp_millis(),
        ))
        .unwrap();
        stored["fallback"] = Value::Bool(false);
        stored.as_object_mut().unwrap().remove("fallback_exhausted");

        let record = serde_json::from_value::<RequestRecord>(stored).unwrap();
        assert_eq!(record.fallback, Some(false));
        assert!(!record.fallback_exhausted);
    }

    #[test]
    fn overview_windows_reconcile_all_models_and_frozen_attempt_costs() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let hour = floor_to(Utc::now().timestamp_millis(), HOUR_MS) - 5 * HOUR_MS;
        // Left edge, full middle hour and right edge are disjoint; endpoints are [from,to).
        for (index, minute) in [9, 10, 59, 60, 119, 120, 129, 130].into_iter().enumerate() {
            let at = hour + minute * MINUTE_MS;
            let mut request = request_record(&format!("r-{index}"), at);
            request.served_model = match index % 3 {
                0 => Some("served".into()),
                1 => Some("deleted-model".into()),
                _ => None,
            };
            request.fallback = Some(index == 1);
            store.record_request(&request).unwrap();
            store.record_request(&request).unwrap();
            let mut attempt = attempt_record(&format!("a-{index}"), at);
            attempt.backend = if index % 2 == 0 {
                "provider"
            } else {
                "deleted-provider"
            }
            .into();
            attempt.completed_at_ms = hour + 200 * MINUTE_MS; // Membership follows start, not end.
            if index % 2 == 0 {
                attempt.error_class = Some(crate::types::FailureClass::ProviderQuota);
            }
            attempt.provider_cost_usd = Some(0.125);
            store.record_attempt(&attempt).unwrap();
            store.record_attempt(&attempt).unwrap();
        }
        let from = hour + 10 * MINUTE_MS;
        let to = hour + 130 * MINUTE_MS;
        let result = store.overview_rollup_window(Some(from), Some(to)).unwrap();
        assert_eq!(result.requests.calls, 6);
        assert_eq!(result.requests.fallbacks, 1);
        assert_eq!(result.requests.total_tokens, 78);
        assert_eq!(result.attempts.len(), 2);
        for value in result.attempts.values() {
            assert_eq!(value.attempts, 3);
            assert_eq!(value.cache_reported_attempts, 3);
            assert_eq!(value.cache_hit_tokens, 12);
            assert_eq!(value.cache_miss_tokens, 18);
            assert_eq!(value.cost_usd, 0.375);
        }
        assert_eq!(
            result.attempts[&("provider".into(), "upstream".into())].errors,
            3
        );
        assert_eq!(
            result.attempts[&("deleted-provider".into(), "upstream".into())].successes,
            3
        );
        for (from, to, expected) in [(60, 120, 2), (10, 11, 1), (10, 10, 0), (0, 180, 8)] {
            let result = store
                .overview_rollup_window(Some(hour + from * MINUTE_MS), Some(hour + to * MINUTE_MS))
                .unwrap();
            assert_eq!(result.requests.calls, expected);
            assert_eq!(
                result.attempts.values().map(|v| v.attempts).sum::<u64>(),
                expected
            );
        }
        assert!(store.overview_rollup_window(Some(to), Some(from)).is_err());
        assert!(store.overview_rollup_window(None, Some(to)).is_err());
        assert_eq!(store.overview_rollup().unwrap().requests.calls, 8);
        store.prune_stats(hour + 120 * MINUTE_MS).unwrap();
        let pruned = store
            .overview_rollup_window(Some(hour), Some(hour + 180 * MINUTE_MS))
            .unwrap();
        assert_eq!(pruned.requests.calls, 3);
        assert_eq!(pruned.attempts.values().map(|v| v.attempts).sum::<u64>(), 3);
        assert_eq!(store.overview_rollup().unwrap().requests.calls, 8);
        assert_eq!(
            store
                .overview_rollup()
                .unwrap()
                .attempts
                .values()
                .map(|v| v.attempts)
                .sum::<u64>(),
            8
        );
    }

    #[test]
    fn aggregates_upstream_errors_by_retained_type_and_code() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let to = floor_to(Utc::now().timestamp_millis(), MINUTE_MS);
        let mut failure = attempt_record("context-limit", to - MINUTE_MS);
        failure.status = Some(400);
        failure.error_class = Some(FailureClass::ClientRequest);
        failure.upstream_error_type = Some("invalid_request_error".into());
        failure.upstream_error_code = Some("invalid_request_error".into());
        failure.upstream_error_message = Some("maximum context length is 1048576 tokens".into());
        failure.sanitized_error = Some("upstream request failed".into());
        store.record_attempt(&failure).unwrap();
        let mut repeat = attempt_record("context-limit-2", to - 2 * MINUTE_MS);
        repeat.status = Some(400);
        repeat.error_class = Some(FailureClass::ClientRequest);
        repeat.upstream_error_type = Some("invalid_request_error".into());
        repeat.upstream_error_code = Some("invalid_request_error".into());
        repeat.upstream_error_message = Some("older sample".into());
        store.record_attempt(&repeat).unwrap();
        let mut other_model = attempt_record("zhipu-limit", to - MINUTE_MS);
        other_model.backend = "zhipu-coding-plan".into();
        other_model.upstream_model = "glm-5.3-flash".into();
        other_model.served_model = Some("glm-5.3-flash".into());
        other_model.status = Some(400);
        other_model.error_class = Some(FailureClass::ClientRequest);
        other_model.upstream_error_type = Some("invalid_request_error".into());
        other_model.upstream_error_code = Some("1210".into());
        other_model.upstream_error_message = Some("max_tokens parameter is illegal".into());
        store.record_attempt(&other_model).unwrap();
        store
            .record_attempt(&attempt_record("success", to - MINUTE_MS))
            .unwrap();
        store
            .record_attempt(&attempt_record("old-failure", to - 48 * HOUR_MS))
            .unwrap();

        let day = store
            .upstream_errors(Some(to - 24 * HOUR_MS), Some(to), None, 12)
            .unwrap();
        assert_eq!(day.len(), 2, "successful and out-of-window rows stay out");
        assert_eq!(
            day[0].upstream_code.as_deref(),
            Some("invalid_request_error")
        );
        assert_eq!(day[0].count, 2, "same type/code pair aggregates together");
        assert_eq!(
            day[0].message.as_deref(),
            Some("maximum context length is 1048576 tokens"),
            "newest sample supplies the message"
        );
        assert_eq!(day[1].upstream_code.as_deref(), Some("1210"));
        assert_eq!(day[1].backend.as_deref(), Some("zhipu-coding-plan"));

        let scoped = store
            .upstream_errors(Some(to - 24 * HOUR_MS), Some(to), Some("glm-5.3-flash"), 12)
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].count, 1);

        // Rows recorded before upstream detail existed still separate by class
        // instead of collapsing into one anonymous bucket.
        let mut legacy = attempt_record("legacy-transport", to - MINUTE_MS);
        legacy.status = Some(502);
        legacy.error_class = Some(FailureClass::ProviderTransient);
        legacy.upstream_error_type = None;
        legacy.upstream_error_code = None;
        legacy.upstream_error_message = None;
        legacy.sanitized_error = Some("upstream transport failure".into());
        store.record_attempt(&legacy).unwrap();
        let mut legacy_client = attempt_record("legacy-client", to - MINUTE_MS);
        legacy_client.status = Some(400);
        legacy_client.error_class = Some(FailureClass::ClientRequest);
        legacy_client.upstream_error_type = None;
        legacy_client.upstream_error_code = None;
        legacy_client.upstream_error_message = None;
        legacy_client.sanitized_error = Some("upstream request failed".into());
        store.record_attempt(&legacy_client).unwrap();
        let legacy_buckets = store
            .upstream_errors(Some(to - 24 * HOUR_MS), Some(to), None, 12)
            .unwrap();
        assert_eq!(legacy_buckets.len(), 4);
        let classes = legacy_buckets
            .iter()
            .filter(|bucket| bucket.upstream_code.is_none())
            .map(|bucket| bucket.error_class)
            .collect::<Vec<_>>();
        assert!(classes.contains(&Some(FailureClass::ProviderTransient)));
        assert!(classes.contains(&Some(FailureClass::ClientRequest)));
    }

    #[test]
    fn day_window_excludes_old_rows_but_lifetime_and_reopen_preserve_them() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        let to = floor_to(Utc::now().timestamp_millis(), MINUTE_MS);
        for (id, at) in [
            ("old", to - 33 * 24 * HOUR_MS),
            ("two-days", to - 48 * HOUR_MS),
            ("today", to - MINUTE_MS),
            ("current-minute", to),
        ] {
            store.record_request(&request_record(id, at)).unwrap();
            store.record_attempt(&attempt_record(id, at)).unwrap();
        }
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        store.ensure_stats_schema().unwrap();
        let day = store
            .overview_rollup_window(Some(to - 24 * HOUR_MS), Some(to))
            .unwrap();
        assert_eq!(day.requests.calls, 1);
        assert_eq!(day.attempts.values().next().unwrap().attempts, 1);
        assert_eq!(day.attempts.values().next().unwrap().cost_usd, 0.25);
        let all = store.overview_rollup_window(None, None).unwrap();
        assert_eq!(all.requests.calls, 4);
        assert_eq!(all.attempts.values().next().unwrap().attempts, 4);
        assert_eq!(all.attempts.values().next().unwrap().cost_usd, 1.0);
    }

    #[test]
    fn existing_v1_attempt_bucket_migration_resumes_without_touching_lifetime_totals() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let at = floor_to(Utc::now().timestamp_millis(), MINUTE_MS) - MINUTE_MS;
        let transaction = store.database.begin_write().unwrap();
        // Simulate an existing v1 store: original all-time backfill is already ready.
        for index in 0..=MIGRATION_BATCH_SIZE {
            let record = attempt_record(&format!("legacy-{index:05}"), at);
            let bytes = serde_json::to_vec(&record).unwrap();
            transaction
                .open_table(ATTEMPTS)
                .unwrap()
                .insert(record.id.as_str(), bytes.as_slice())
                .unwrap();
            apply_attempt_rollup(&transaction, &record).unwrap();
        }
        write_stats_meta(
            &transaction,
            &StatsMeta {
                phase: StatsPhase::Ready,
                ..StatsMeta::default()
            },
        )
        .unwrap();
        transaction.commit().unwrap();
        assert!(
            store
                .record_attempt(&attempt_record("blocked", at))
                .is_err()
        );
        assert!(
            store
                .record_request(&request_record("blocked", at))
                .is_err()
        );
        let mut meta = AttemptBucketMeta::default();
        store.backfill_attempt_buckets_batch(&mut meta).unwrap();
        assert!(!meta.ready);
        assert!(meta.cursor.is_some());
        assert!(
            store
                .record_attempt(&attempt_record("still-blocked", at))
                .is_err()
        );
        assert_eq!(
            store
                .overview_rollup_window(Some(at), Some(at + MINUTE_MS))
                .unwrap()
                .attempts
                .values()
                .next()
                .unwrap()
                .attempts,
            MIGRATION_BATCH_SIZE as u64
        );
        drop(store);
        let store = Store::open(dir.path()).unwrap();
        store.ensure_stats_schema().unwrap();
        store.ensure_stats_schema().unwrap();
        let count = (MIGRATION_BATCH_SIZE + 1) as u64;
        let all = store.overview_rollup().unwrap();
        let bounded = store
            .overview_rollup_window(Some(at), Some(at + MINUTE_MS))
            .unwrap();
        assert_eq!(all.attempts.values().next().unwrap().attempts, count);
        assert_eq!(bounded.attempts, all.attempts);
        store
            .record_attempt(&attempt_record("legacy-00000", at))
            .unwrap();
        store.record_attempt(&attempt_record("new", at)).unwrap();
        assert_eq!(
            store
                .overview_rollup_window(Some(at), Some(at + MINUTE_MS))
                .unwrap()
                .attempts
                .values()
                .next()
                .unwrap()
                .attempts,
            count + 1
        );
        assert_eq!(store.attempts(2000).unwrap().len(), count as usize + 1);
    }

    fn request_record(id: &str, started_at_ms: i64) -> RequestRecord {
        RequestRecord {
            id: id.into(),
            started_at_ms,
            completed_at_ms: started_at_ms + 1,
            protocol: Protocol::OpenAiChat,
            requested_model: "served".into(),
            served_model: Some("served".into()),
            upstream_model: Some("upstream".into()),
            streaming: false,
            status: 200,
            error_class: None,
            backend: Some("provider".into()),
            adapter: Some(AdapterKind::DeepSeekOfficial),
            credential: Some("key-a".into()),
            route_layer: Some("plan".into()),
            route_layer_index: Some(0),
            selection_reason: Some("single-target".into()),
            matched_prefix_bytes: None,
            fallback: Some(false),
            fallback_exhausted: false,
            translated: false,
            request_bytes: 100,
            response_bytes: 200,
            first_byte_ms: Some(1),
            total_ms: 2,
            usage: Usage {
                input_tokens: 10,
                cache_hit_tokens: 4,
                cache_miss_tokens: 6,
                reasoning_tokens: 2,
                reasoning_reported: true,
                reasoning_estimated_tokens: 0,
                output_tokens: 3,
                total_tokens: 13,
                provider_reported: true,
            },
            claude_session_id: None,
            claude_agent_id: None,
            claude_parent_agent_id: None,
            upstream_error_type: None,
            upstream_error_code: None,
            upstream_error_message: None,
        }
    }

    fn attempt_record(id: &str, started_at_ms: i64) -> AttemptRecord {
        AttemptRecord {
            id: id.into(),
            request_id: Some("legacy-request".into()),
            sequence: 1,
            backend: "provider".into(),
            adapter: Some(AdapterKind::DeepSeekOfficial),
            credential: Some("key-a".into()),
            route_layer: Some("plan".into()),
            route_layer_index: Some(0),
            selection_reason: Some("single-target".into()),
            matched_prefix_bytes: None,
            upstream_model: "upstream".into(),
            served_model: Some("served".into()),
            egress_protocol: Protocol::OpenAiChat,
            translated: false,
            probe: false,
            started_at_ms,
            completed_at_ms: started_at_ms + 1,
            status: Some(200),
            error_class: None,
            retry_after_ms: None,
            committed: true,
            request_bytes: 100,
            response_bytes: 200,
            first_byte_ms: Some(1),
            total_ms: 2,
            usage: request_record("usage", started_at_ms).usage,
            provider_cost_usd: Some(0.25),
            sanitized_error: None,
            upstream_error_type: None,
            upstream_error_code: None,
            upstream_error_message: None,
        }
    }
}
