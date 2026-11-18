use crate::{Error, validate_name};
use chrono::{DateTime, Utc};
use croner::Cron;
use croner::parser::{CronParser, Seconds, Year};
use serde_json::Value as Json;
use std::time::Duration;

/// The largest number of occurrences of one cron trigger a worker records at once.
pub(crate) const MAX_CATCH_UP: usize = 1000;

/// A trigger that fires at the times of a cron expression, in UTC.
#[derive(Debug, Clone)]
pub struct CronTrigger {
    pub(crate) name: String,
    pub(crate) schedule: Cron,
    pub(crate) payload: Json,
    pub(crate) tolerance: Duration,
}

impl CronTrigger {
    /// A trigger named `name` for the five-field cron expression `expression` (minute, hour,
    /// day of month, month, day of week), whose events carry `payload`.
    pub fn new(name: impl Into<String>, expression: &str, payload: Json) -> Result<Self, Error> {
        let name = name.into();
        validate_name(&name)?;
        if expression.split_whitespace().count() != 5 {
            return Err(Error::Definition(format!(
                "{name}: a cron expression has five fields"
            )));
        }
        let schedule = CronParser::builder()
            .seconds(Seconds::Disallowed)
            .year(Year::Disallowed)
            .build()
            .parse(expression)
            .map_err(|e| Error::Definition(format!("{name}: invalid cron expression: {e}")))?;
        Ok(Self {
            name,
            schedule,
            payload,
            tolerance: Duration::from_secs(6 * 60 * 60),
        })
    }

    /// How old a missed occurrence may be when a worker records it. Defaults to six
    /// hours.
    pub fn tolerance(mut self, tolerance: Duration) -> Self {
        self.tolerance = tolerance;
        self
    }

    pub(crate) fn occurrences(
        &self,
        after: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> Vec<DateTime<Utc>> {
        self.schedule
            .iter_after(after)
            .take_while(|time| *time <= until)
            .take(MAX_CATCH_UP)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    #[test]
    fn expressions_have_five_fields() {
        assert!(CronTrigger::new("a", "*/5 * * * *", Json::Null).is_ok());
        for expression in ["* * * *", "0 */5 * * * *", "61 * * * *", "@reboot", ""] {
            assert!(
                CronTrigger::new("a", expression, Json::Null).is_err(),
                "{expression}"
            );
        }
        assert!(CronTrigger::new("a b", "* * * * *", Json::Null).is_err());
    }

    #[test]
    fn occurrences_are_after_the_start_and_up_to_the_end() {
        let cron = CronTrigger::new("a", "*/5 * * * *", Json::Null).unwrap();
        assert_eq!(
            cron.occurrences(time("2026-01-01T00:05:00Z"), time("2026-01-01T00:15:00Z")),
            vec![time("2026-01-01T00:10:00Z"), time("2026-01-01T00:15:00Z")]
        );
        assert!(
            cron.occurrences(time("2026-01-01T00:05:00Z"), time("2026-01-01T00:09:59Z"))
                .is_empty()
        );
        assert_eq!(
            cron.occurrences(time("2000-01-01T00:00:00Z"), time("2026-01-01T00:00:00Z"))
                .len(),
            MAX_CATCH_UP
        );
    }
}
