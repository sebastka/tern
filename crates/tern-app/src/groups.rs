//! Date sections of a message list sorted by date: "Today", "Yesterday",
//! "This week", "Last week", then calendar months.

use std::collections::HashMap;

use chrono::{Datelike, Days, Local, NaiveDate};
use tern_core::MessageId;
use tern_core::thread::ThreadRow;

use crate::types::{DateGroup, ListGroup};

/// The section of a message dated `date` (Unix seconds), seen on `today`
/// (local time). Weeks start on Monday; future dates count as today.
pub fn classify(date: i64, today: NaiveDate) -> (DateGroup, i32, u32) {
    let d = chrono::DateTime::from_timestamp(date, 0).unwrap_or_default().with_timezone(&Local).date_naive();
    let week_start = today - Days::new(today.weekday().num_days_from_monday().into());
    if d >= today {
        (DateGroup::Today, 0, 0)
    } else if Some(d) == today.pred_opt() {
        (DateGroup::Yesterday, 0, 0)
    } else if d >= week_start {
        (DateGroup::ThisWeek, 0, 0)
    } else if d >= week_start - Days::new(7) {
        (DateGroup::LastWeek, 0, 0)
    } else {
        (DateGroup::Month, d.year(), d.month())
    }
}

/// Section starts for `rows`. In a threaded list a thread (a root row and
/// the deeper rows after it) is one unit, dated by its newest message,
/// matching how threads are ordered.
pub fn date_groups(
    rows: &[ThreadRow],
    dates: &HashMap<MessageId, i64>,
    threaded: bool,
    today: NaiveDate,
) -> Vec<ListGroup> {
    let mut groups: Vec<ListGroup> = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        let end = if threaded { i + 1 + rows[i + 1..].iter().take_while(|r| r.depth > 0).count() } else { i + 1 };
        let date = rows[i..end].iter().filter_map(|r| dates.get(&r.id)).max().copied().unwrap_or(0);
        let (kind, year, month) = classify(date, today);
        if groups.last().is_none_or(|g| (g.kind, g.year, g.month) != (kind, year, month)) {
            groups.push(ListGroup { start: i as u32, kind, year, month });
        }
        i = end;
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Noon local time on a date, as Unix seconds.
    fn at(y: i32, m: u32, d: u32) -> i64 {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_local_timezone(Local)
            .unwrap()
            .timestamp()
    }

    #[test]
    fn sections() {
        // Wednesday 7 October 2026.
        let today = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        assert_eq!(classify(at(2026, 10, 7), today).0, DateGroup::Today);
        assert_eq!(classify(at(2026, 10, 9), today).0, DateGroup::Today);
        assert_eq!(classify(at(2026, 10, 6), today).0, DateGroup::Yesterday);
        assert_eq!(classify(at(2026, 10, 5), today).0, DateGroup::ThisWeek); // Monday
        assert_eq!(classify(at(2026, 10, 4), today).0, DateGroup::LastWeek); // Sunday
        assert_eq!(classify(at(2026, 9, 28), today).0, DateGroup::LastWeek);
        assert_eq!(classify(at(2026, 9, 27), today), (DateGroup::Month, 2026, 9));
        assert_eq!(classify(at(2025, 12, 31), today), (DateGroup::Month, 2025, 12));
    }

    #[test]
    fn starts_flat_and_threaded() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        let row = |id, depth| ThreadRow { id, depth, thread_size: 0 };
        let dates: HashMap<i64, i64> = [
            (1, at(2026, 10, 7)),
            (2, at(2026, 10, 7)),
            (3, at(2026, 10, 6)),
            (4, at(2026, 8, 1)),
            (5, at(2026, 8, 2)),
        ]
        .into();
        let flat: Vec<ThreadRow> = (1..=5).map(|i| row(i, 0)).collect();
        let g = date_groups(&flat, &dates, false, today);
        assert_eq!(
            g.iter().map(|g| (g.start, g.kind)).collect::<Vec<_>>(),
            [(0, DateGroup::Today), (2, DateGroup::Yesterday), (3, DateGroup::Month)]
        );
        // A thread whose reply is from today belongs to Today as a whole.
        let threaded = [row(4, 0), row(1, 1), row(3, 0), row(5, 0)];
        let g = date_groups(&threaded, &dates, true, today);
        assert_eq!(
            g.iter().map(|g| (g.start, g.kind)).collect::<Vec<_>>(),
            [(0, DateGroup::Today), (2, DateGroup::Yesterday), (3, DateGroup::Month)]
        );
        assert!(date_groups(&[], &dates, false, today).is_empty());
    }
}
