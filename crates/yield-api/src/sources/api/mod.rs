pub mod catalog;
pub mod defillama;

use anyhow::{bail, ensure, Result};
use serde_json::Value;

use crate::{
    config::SourceConfig,
    model::{Evidence, Observation, Origin},
    sources::{bounded_json, Context},
};

const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORDS: usize = 20_000;

pub async fn collect(source: &SourceConfig, context: &Context<'_>) -> Result<Vec<Observation>> {
    match source {
        SourceConfig::Defillama { id } => {
            let payload = fetch(context, defillama::POOLS_URL).await?;
            defillama::parse(&payload, id, context.collected_at_ms)
        }
        SourceConfig::CatalogApi { id, url_env } => {
            let url = std::env::var(url_env)
                .map_err(|_| anyhow::anyhow!("catalog URL is not configured"))?;
            let url =
                reqwest::Url::parse(&url).map_err(|_| anyhow::anyhow!("catalog URL is invalid"))?;
            ensure!(
                url.scheme() == "https"
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none(),
                "catalog URL must be HTTPS without user information or a fragment"
            );
            let payload = fetch(context, url.as_str()).await?;
            catalog::parse(&payload, id, context.collected_at_ms)
        }
        _ => bail!("unsupported API adapter"),
    }
}

async fn fetch(context: &Context<'_>, url: &str) -> Result<Value> {
    // The shared client disables redirects. Do not put a request error or URL
    // in a returned error: a configured URL can contain a credential.
    let response = context
        .http
        .get(url)
        .send()
        .await
        .map_err(|_| anyhow::anyhow!("API source request failed"))?;
    bounded_json(response, MAX_JSON_BYTES)
        .await
        .map_err(|_| anyhow::anyhow!("API source returned an invalid or oversized response"))
}

pub(super) fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.len() <= 512)
}

pub(super) fn identity(row: &Value, fields: &[&str]) -> Option<String> {
    fields
        .iter()
        .find_map(|field| text(row.get(*field)))
        .map(str::to_owned)
}

pub(super) fn number(value: &Value) -> Option<f64> {
    // Strings, booleans, null, and non-finite numbers are not rates.
    value.as_f64().filter(|n| n.is_finite())
}

pub(super) fn evidence() -> Evidence {
    Evidence {
        origin: Origin::Api,
        transport: "https_json".into(),
        genesis_hash: None,
        commitment: None,
        context_slot: None,
        accounts: Vec::new(),
    }
}

pub(super) fn unavailable(observation: &mut Observation, reason: &str) {
    observation.rates.clear();
    observation.unavailable_reason = Some(reason.into());
}

/// Only a timestamp attached to the row can establish its source time.
/// In particular, envelope `generated_at`, HTTP Date, and collection time
/// cannot establish when the provider measured a rate.
pub(super) fn source_time(row: &Value, collected_at_ms: u64) -> Option<u64> {
    let candidate = if let Some(value) = row.get("observed_at_ms") {
        value.as_u64()
    } else if let Some(value) = row.get("observed_at") {
        value.as_u64().and_then(|n| n.checked_mul(1_000))
    } else if let Some(value) = row.get("timestamp") {
        match value {
            Value::String(s) => rfc3339_ms(s),
            _ => value.as_u64().and_then(|n| n.checked_mul(1_000)),
        }
    } else {
        None
    };
    candidate.filter(|at| *at > 0 && *at <= collected_at_ms.saturating_add(30_000))
}

/// RFC3339 with an explicit timezone, checked calendar fields, and up to
/// nanosecond precision. The stored precision is milliseconds.
fn rfc3339_ms(s: &str) -> Option<u64> {
    let bytes = s.as_bytes();
    if bytes.len() < 20
        || !s.is_ascii()
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let digits = |from: usize, to: usize| -> Option<i64> {
        let part = bytes.get(from..to)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        part.iter().try_fold(0_i64, |n, digit| {
            n.checked_mul(10)?.checked_add(i64::from(digit - b'0'))
        })
    };
    let (year, month, day) = (digits(0, 4)?, digits(5, 7)?, digits(8, 10)?);
    let (hour, minute, second) = (digits(11, 13)?, digits(14, 16)?, digits(17, 19)?);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if day < 1 || day > month_days || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let mut end = 19;
    let mut millis = 0;
    if bytes.get(end) == Some(&b'.') {
        let start = end + 1;
        end = start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        let len = end - start;
        if len == 0 || len > 9 {
            return None;
        }
        millis = digits(start, start + len.min(3))? * 10_i64.pow(3 - len.min(3) as u32);
    }
    let offset = match bytes.get(end..) {
        Some(b"Z" | b"z") => 0,
        Some([sign @ (b'+' | b'-'), h1, h2, b':', m1, m2])
            if [h1, h2, m1, m2].iter().all(|b| b.is_ascii_digit()) =>
        {
            let hours = i64::from((h1 - b'0') * 10 + (h2 - b'0'));
            let minutes = i64::from((m1 - b'0') * 10 + (m2 - b'0'));
            if hours > 23 || minutes > 59 {
                return None;
            }
            (hours * 3_600 + minutes * 60) * if *sign == b'+' { 1 } else { -1 }
        }
        _ => return None,
    };
    // Proleptic Gregorian calendar, days relative to 1970-01-01.
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * shifted_month + 2) / 5 + day - 1;
    let days = era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468;
    let seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset;
    u64::try_from(seconds.checked_mul(1_000)?.checked_add(millis)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn original_times_are_strict_and_never_use_envelope_or_fetch_time() {
        let now = 1_792_108_800_000;
        assert_eq!(source_time(&json!({"generated_at": now / 1000}), now), None);
        assert_eq!(
            source_time(
                &json!({"observed_at": null,"timestamp":"2026-10-16T00:00:00Z"}),
                now
            ),
            None
        );
        assert_eq!(source_time(&json!({"observed_at": 0}), now), None);
        assert_eq!(source_time(&json!({"observed_at": -1}), now), None);
        assert_eq!(source_time(&json!({"observed_at": 1.5}), now), None);
        assert_eq!(
            source_time(&json!({"observed_at_ms": now + 30_001}), now),
            None
        );
        assert_eq!(
            source_time(&json!({"observed_at": now / 1000}), now),
            Some(now)
        );
    }

    #[test]
    fn rfc3339_requires_valid_calendar_and_timezone() {
        assert_eq!(rfc3339_ms("1970-01-01T00:00:01Z"), Some(1_000));
        assert_eq!(
            rfc3339_ms("2024-02-29T02:03:04.123456789+02:00"),
            rfc3339_ms("2024-02-29T00:03:04.123Z")
        );
        assert_eq!(rfc3339_ms("2026-10-16T00:00:00Z"), Some(1_792_108_800_000));
        for bad in [
            "2023-02-29T00:00:00Z",
            "2024-04-31T00:00:00Z",
            "2024-02-29T24:00:00Z",
            "2024-02-29T00:00:60Z",
            "2024-02-29T00:00:00",
            "2024-02-29T00:00:00.Z",
            "2024-02-29T00:00:00+24:00",
            "2024-02-29T00:00:00Zsuffix",
        ] {
            assert_eq!(rfc3339_ms(bad), None, "{bad}");
        }
    }
}
