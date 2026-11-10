// Copyright (c) Microsoft Corporation.
// Licensed under the MIT License.

/// Keep Bridge's existing chrono formatting and elapsed-time semantics while
/// Kubernetes metadata uses jiff. Jiff's supported year range fits in chrono;
/// its negative fractional seconds need normalization to chrono's unsigned nanos.
pub(crate) fn to_chrono(timestamp: k8s_openapi::jiff::Timestamp) -> chrono::DateTime<chrono::Utc> {
    let nanos = timestamp.subsec_nanosecond();
    chrono::DateTime::from_timestamp(
        timestamp.as_second() + i64::from(nanos.div_euclid(1_000_000_000)),
        nanos.rem_euclid(1_000_000_000) as u32,
    )
    .expect("jiff timestamps are within chrono's supported range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kubernetes_timestamps_preserve_bridge_rfc3339_format_and_precision() {
        for value in [
            "1970-01-01T00:00:00Z",
            "2026-11-04T18:22:02Z",
            "2026-11-04T18:22:02.123456789+01:00",
            "1969-12-31T23:59:59.123456789Z",
            "1960-01-01T00:00:00.999999999Z",
        ] {
            let expected = chrono::DateTime::parse_from_rfc3339(value)
                .unwrap()
                .with_timezone(&chrono::Utc);
            let actual = to_chrono(value.parse().unwrap());
            assert_eq!(actual, expected);
            assert_eq!(actual.to_rfc3339(), expected.to_rfc3339());
        }
        for timestamp in [
            k8s_openapi::jiff::Timestamp::MIN,
            k8s_openapi::jiff::Timestamp::MAX,
        ] {
            // Chrono and jiff format extended negative years differently.
            // Compare the full-range instant without a formatting round trip.
            let actual = to_chrono(timestamp);
            assert_eq!(
                i128::from(actual.timestamp()) * 1_000_000_000
                    + i128::from(actual.timestamp_subsec_nanos()),
                timestamp.as_nanosecond(),
            );
        }
    }

    #[test]
    fn kubernetes_timestamp_age_keeps_subsecond_truncation_and_future_clamp() {
        let now = to_chrono("2026-11-04T18:22:02.100Z".parse().unwrap());
        for (value, seconds) in [
            ("2026-11-04T18:22:00.900Z", 1),
            ("2026-11-04T18:22:01.900Z", 0),
            ("2026-11-04T18:22:03Z", 0),
        ] {
            assert_eq!(
                (now - to_chrono(value.parse().unwrap()))
                    .num_seconds()
                    .max(0),
                seconds
            );
        }
    }
}
