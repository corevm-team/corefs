use crate::config::StorageTier;
use crate::error::{CoreFsError, CoreFsResult};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VersionQuery {
    Latest,
    VersionId(u64),
    Timestamp(SystemTime),
}

pub(super) fn parse_version_selector(selector: &str) -> CoreFsResult<(&str, VersionQuery)> {
    let (path, suffix) = selector.rsplit_once('@').ok_or_else(|| {
        CoreFsError::InvalidInput(format!("version selector must contain '@': {selector}"))
    })?;
    super::pathing::validate_path(path)?;

    if suffix == "latest" {
        return Ok((path, VersionQuery::Latest));
    }
    if let Some(raw) = suffix.strip_prefix('v') {
        let version_id = raw.parse::<u64>().map_err(|error| {
            CoreFsError::InvalidInput(format!(
                "invalid version id in selector {selector}: {error}"
            ))
        })?;
        return Ok((path, VersionQuery::VersionId(version_id)));
    }

    Ok((
        path,
        VersionQuery::Timestamp(parse_timestamp_selector(suffix)?),
    ))
}

fn parse_timestamp_selector(value: &str) -> CoreFsResult<SystemTime> {
    let normalized = value.replace('T', "-").replace(':', "-");
    let parts = normalized
        .split('-')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 6 {
        return Err(CoreFsError::InvalidInput(format!(
            "invalid timestamp selector: {value}"
        )));
    }

    let year = parse_i64(parts[0], "year", value)?;
    let month = parse_i64(parts[1], "month", value)?;
    let day = parse_i64(parts[2], "day", value)?;
    let hour = parse_i64(parts[3], "hour", value)?;
    let minute = parse_i64(parts[4], "minute", value)?;
    let second = parse_i64(parts[5], "second", value)?;

    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return Err(CoreFsError::InvalidInput(format!(
            "timestamp selector out of range: {value}"
        )));
    }

    let days = days_from_civil(year, month, day)?;
    let total_seconds = days
        .checked_mul(86_400)
        .and_then(|base| base.checked_add(hour * 3_600 + minute * 60 + second))
        .ok_or_else(|| {
            CoreFsError::InvalidInput(format!("timestamp selector overflow: {value}"))
        })?;

    if total_seconds < 0 {
        return Err(CoreFsError::InvalidInput(format!(
            "timestamp selector predates unix epoch: {value}"
        )));
    }

    Ok(UNIX_EPOCH + Duration::from_secs(total_seconds as u64))
}

fn parse_i64(value: &str, label: &str, original: &str) -> CoreFsResult<i64> {
    value.parse::<i64>().map_err(|error| {
        CoreFsError::InvalidInput(format!(
            "invalid {label} in timestamp selector {original}: {error}"
        ))
    })
}

fn days_from_civil(year: i64, month: i64, day: i64) -> CoreFsResult<i64> {
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let yoe = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * month_prime + 2) / 5 + day - 1;
    let max_day = match month {
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
            if leap { 29 } else { 28 }
        }
        _ => 31,
    };
    if day > max_day {
        return Err(CoreFsError::InvalidInput(format!(
            "invalid day {day} for month {month} in timestamp selector"
        )));
    }
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Ok(era * 146_097 + doe - 719_468)
}

pub(super) fn tier_name(tier: &StorageTier) -> &'static str {
    match tier {
        StorageTier::Hot => "hot",
        StorageTier::Warm => "warm",
        StorageTier::Cold => "cold",
    }
}
