//! Message list order from `[ui.message_list]` (`sort_by`, `sort_order`).
//!
//! The default (date, newest first) is what the store and the threading
//! code produce already; other orders re-sort their output here.

use std::cmp::Ordering;
use std::collections::HashMap;

use tern_config::{ListField, SortOrder};
use tern_core::store::SortInput;
use tern_core::thread::ThreadRow;
use tern_core::{Flags, MessageId};

use crate::types::ListColumn;

impl From<ListField> for ListColumn {
    fn from(f: ListField) -> Self {
        match f {
            ListField::Flag => Self::Flag,
            ListField::Subject => Self::Subject,
            ListField::From => Self::From,
            ListField::To => Self::To,
            ListField::Correspondent => Self::Correspondent,
            ListField::Date => Self::Date,
            ListField::Attachment => Self::Attachment,
            ListField::Size => Self::Size,
        }
    }
}

/// Is this the order the store and threading already produce?
pub fn is_default(field: ListField, order: SortOrder) -> bool {
    field == ListField::Date && order == SortOrder::Desc
}

/// A message's value for one field: numbers compare as numbers, text
/// case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Value {
    Num(i64),
    Text(String),
}

/// `outgoing`: the folder holds sent mail or drafts, so the correspondent
/// is the recipient.
fn value(m: &SortInput, field: ListField, outgoing: bool) -> Value {
    let text = |s: &str| Value::Text(s.to_lowercase());
    match field {
        ListField::Date => Value::Num(m.date),
        ListField::Size => Value::Num(m.size.into()),
        // Flagged first, then unread (in descending order).
        ListField::Flag => {
            Value::Num(i64::from(m.flags.contains(Flags::FLAGGED)) * 2 + i64::from(!m.flags.contains(Flags::SEEN)))
        }
        ListField::Attachment => Value::Num(i64::from(m.encrypted) * 2 + i64::from(m.has_attachments)),
        ListField::Subject => Value::Text(base_subject(&m.subject)),
        ListField::From => text(&m.from),
        ListField::To => text(&m.to),
        ListField::Correspondent => text(if outgoing { &m.to } else { &m.from }),
    }
}

/// The subject without reply/forward prefixes, lowercased, so a reply sorts
/// next to its original.
fn base_subject(s: &str) -> String {
    const PREFIXES: [&str; 7] = ["re", "fwd", "fw", "aw", "sv", "vs", "wg"];
    let mut s = s.trim();
    'strip: loop {
        for p in PREFIXES {
            if let Some(head) = s.get(..p.len())
                && head.eq_ignore_ascii_case(p)
                && let Some(rest) = s[p.len()..].strip_prefix(':')
            {
                s = rest.trim_start();
                continue 'strip;
            }
        }
        break;
    }
    s.to_lowercase()
}

/// Compare by the key in the requested order, ties newest first.
fn compare(a: &(Value, i64, MessageId), b: &(Value, i64, MessageId), order: SortOrder) -> Ordering {
    let primary = match order {
        SortOrder::Asc => a.0.cmp(&b.0),
        SortOrder::Desc => b.0.cmp(&a.0),
    };
    primary.then(b.1.cmp(&a.1)).then(b.2.cmp(&a.2))
}

/// Sort message ids (flat list or search results). Ids without input are
/// dropped.
pub fn sort_flat(
    ids: Vec<MessageId>,
    inputs: &[SortInput],
    field: ListField,
    order: SortOrder,
    outgoing: bool,
) -> Vec<MessageId> {
    let by_id: HashMap<MessageId, &SortInput> = inputs.iter().map(|m| (m.id, m)).collect();
    let mut keyed: Vec<(Value, i64, MessageId)> =
        ids.into_iter().filter_map(|id| by_id.get(&id).map(|m| (value(m, field, outgoing), m.date, id))).collect();
    keyed.sort_by(|a, b| compare(a, b, order));
    keyed.into_iter().map(|(_, _, id)| id).collect()
}

/// Reorder whole threads; messages inside a thread keep their order.
/// Numeric fields use the thread's highest value (newest date, any message
/// flagged...), text fields the thread's first message.
pub fn sort_threads(
    rows: Vec<ThreadRow>,
    inputs: &[SortInput],
    field: ListField,
    order: SortOrder,
    outgoing: bool,
) -> Vec<ThreadRow> {
    let by_id: HashMap<MessageId, &SortInput> = inputs.iter().map(|m| (m.id, m)).collect();
    let mut threads: Vec<Vec<ThreadRow>> = Vec::new();
    for r in rows {
        match threads.last_mut() {
            Some(t) if r.depth > 0 => t.push(r),
            _ => threads.push(vec![r]),
        }
    }
    let mut keyed: Vec<((Value, i64, MessageId), Vec<ThreadRow>)> = threads
        .into_iter()
        .map(|t| {
            let members: Vec<&SortInput> = t.iter().filter_map(|r| by_id.get(&r.id).copied()).collect();
            let newest = members.iter().map(|m| m.date).max().unwrap_or(0);
            let key = match members.first() {
                None => Value::Num(0),
                Some(first) => match value(first, field, outgoing) {
                    Value::Text(s) => Value::Text(s),
                    Value::Num(_) => members.iter().map(|m| value(m, field, outgoing)).max().unwrap_or(Value::Num(0)),
                },
            };
            ((key, newest, t[0].id), t)
        })
        .collect();
    keyed.sort_by(|a, b| compare(&a.0, &b.0, order));
    keyed.into_iter().flat_map(|(_, t)| t).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(id: MessageId, date: i64, subject: &str, from: &str, flags: Flags) -> SortInput {
        SortInput {
            id,
            date,
            subject: subject.into(),
            from: from.into(),
            to: format!("to{id}"),
            size: (id * 10) as u32,
            flags,
            has_attachments: false,
            encrypted: false,
        }
    }

    fn row(id: MessageId, depth: u32) -> ThreadRow {
        ThreadRow { id, depth, thread_size: 0 }
    }

    #[test]
    fn base_subjects() {
        assert_eq!(base_subject("Re: RE:Fwd:  Hello"), "hello");
        assert_eq!(base_subject("SV: Møte"), "møte");
        assert_eq!(base_subject("Reply needed"), "reply needed");
        assert_eq!(base_subject("Ré: x"), "ré: x");
    }

    #[test]
    fn flat_orders() {
        let inputs = vec![
            input(1, 100, "b", "Zoe", Flags::SEEN),
            input(2, 300, "Re: a", "adam", Flags::SEEN),
            input(3, 200, "c", "Mia", Flags(Flags::SEEN.0 | Flags::FLAGGED.0)),
            input(4, 50, "A", "bob", Flags::empty()),
        ];
        let sort = |f, o| sort_flat(vec![1, 2, 3, 4, 99], &inputs, f, o, false);
        assert_eq!(sort(ListField::Date, SortOrder::Asc), [4, 1, 3, 2]);
        assert_eq!(sort(ListField::Date, SortOrder::Desc), [2, 3, 1, 4]);
        // "Re: a" and "A" tie: newest first.
        assert_eq!(sort(ListField::Subject, SortOrder::Asc), [2, 4, 1, 3]);
        assert_eq!(sort(ListField::From, SortOrder::Asc), [2, 4, 3, 1]);
        assert_eq!(sort(ListField::Correspondent, SortOrder::Desc), [1, 3, 4, 2]);
        assert_eq!(sort(ListField::Flag, SortOrder::Desc), [3, 4, 2, 1]);
        assert_eq!(sort(ListField::Size, SortOrder::Asc), [1, 2, 3, 4]);
        // Outgoing folders use the recipients.
        assert_eq!(sort_flat(vec![1, 2], &inputs, ListField::Correspondent, SortOrder::Desc, true), [2, 1]);
    }

    #[test]
    fn threads_move_as_a_whole() {
        let inputs = vec![
            input(1, 100, "Zebra", "x", Flags::SEEN),
            input(2, 400, "Re: Zebra", "y", Flags::empty()),
            input(3, 300, "apple", "z", Flags::SEEN),
            input(4, 200, "Mango", "w", Flags::SEEN),
        ];
        let rows = vec![row(1, 0), row(2, 1), row(3, 0), row(4, 0)];
        let order = |f, o| sort_threads(rows.clone(), &inputs, f, o, false).iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(order(ListField::Subject, SortOrder::Asc), [3, 4, 1, 2]);
        // Thread "Zebra" has the newest message (400): last when ascending.
        assert_eq!(order(ListField::Date, SortOrder::Asc), [4, 3, 1, 2]);
        // The reply is unread, so the whole thread goes first.
        assert_eq!(order(ListField::Flag, SortOrder::Desc), [1, 2, 3, 4]);
        let depths: Vec<u32> = sort_threads(rows.clone(), &inputs, ListField::Subject, SortOrder::Desc, false)
            .iter()
            .map(|r| r.depth)
            .collect();
        assert_eq!(depths, [0, 1, 0, 0]);
    }
}
