//! JWZ message threading (<https://www.jwz.org/doc/threading.html>), computed
//! per folder (ARCHITECTURE.md §10).
//!
//! The result is flattened into display order with a depth per row, so a
//! threaded view is still a plain windowed list for the frontends.
//!
//! Simplification versus the full algorithm: no subject-based grouping of
//! roots. Merging unrelated messages that share a subject ("Hello", "Invoice")
//! is the most common complaint about JWZ, and all modern MUAs set
//! References/In-Reply-To.

use std::collections::HashMap;

use crate::model::MessageId;
use crate::store::ThreadInput;

/// References considered per message (the newest ones).
const MAX_REFERENCES: usize = 50;

/// One row of the threaded list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadRow {
    pub id: MessageId,
    pub depth: u32,
    /// Number of messages in the thread (only set on the root row).
    pub thread_size: u32,
}

#[derive(Default)]
struct Container {
    message: Option<usize>, // index into inputs
    parent: Option<usize>,
    children: Vec<usize>,
}

/// Thread messages. Threads are ordered by their newest message (newest
/// first), messages within a thread by date (oldest first).
pub fn thread(inputs: &[ThreadInput]) -> Vec<ThreadRow> {
    let mut containers: Vec<Container> = Vec::new();
    let mut by_id: HashMap<String, usize> = HashMap::new();

    let mut get = |containers: &mut Vec<Container>, key: &str| -> usize {
        *by_id.entry(key.to_owned()).or_insert_with(|| {
            containers.push(Container::default());
            containers.len() - 1
        })
    };

    for (i, m) in inputs.iter().enumerate() {
        // Messages without Message-ID, or duplicates, get their own container.
        let c = match &m.message_id {
            Some(mid) => {
                let c = get(&mut containers, mid);
                if containers[c].message.is_some() {
                    containers.push(Container::default());
                    containers.len() - 1
                } else {
                    c
                }
            }
            None => {
                containers.push(Container::default());
                containers.len() - 1
            }
        };
        containers[c].message = Some(i);

        // Chain the references: each one is the parent of the next.
        // Only the most recent references matter, and an unbounded list
        // (hostile mail) must not make threading slow.
        let skip = m.references.len().saturating_sub(MAX_REFERENCES);
        let mut refs: Vec<&str> = m.references[skip..].iter().map(String::as_str).collect();
        if let Some(irt) = &m.in_reply_to
            && refs.last() != Some(&irt.as_str())
        {
            refs.push(irt);
        }
        let mut prev: Option<usize> = None;
        for r in &refs {
            let rc = get(&mut containers, r);
            if let Some(p) = prev
                && containers[rc].parent.is_none()
                && p != rc
                && !is_ancestor(&containers, rc, p)
            {
                link(&mut containers, p, rc);
            }
            prev = Some(rc);
        }
        // The message's parent is the last reference; this overrides whatever
        // earlier, possibly truncated, chains said.
        if let Some(p) = prev
            && p != c
            && !is_ancestor(&containers, c, p)
        {
            unlink(&mut containers, c);
            link(&mut containers, p, c);
        }
    }

    // Roots, with empty containers pruned (their children promoted).
    let roots: Vec<usize> = (0..containers.len()).filter(|&c| containers[c].parent.is_none()).collect();
    let dates: HashMap<MessageId, i64> = inputs.iter().map(|m| (m.id, m.date)).collect();
    let mut threads: Vec<(i64, Vec<ThreadRow>)> = Vec::new();
    let earliest = earliest_dates(&containers, inputs, &roots);
    for root in roots {
        let mut rows = Vec::new();
        flatten(&containers, inputs, &earliest, root, &mut rows);
        if rows.is_empty() {
            continue;
        }
        let newest = rows.iter().map(|r| dates[&r.id]).max().unwrap_or(0);
        rows[0].thread_size = rows.len() as u32;
        threads.push((newest, rows));
    }
    threads.sort_by(|a, b| b.0.cmp(&a.0).then(b.1[0].id.cmp(&a.1[0].id)));
    threads.into_iter().flat_map(|(_, rows)| rows).collect()
}

fn is_ancestor(cs: &[Container], ancestor: usize, of: usize) -> bool {
    let mut cur = Some(of);
    let mut steps = 0;
    while let Some(c) = cur {
        if c == ancestor {
            return true;
        }
        cur = cs[c].parent;
        steps += 1;
        if steps > cs.len() {
            return true; // defensive: treat a cycle as ancestry
        }
    }
    false
}

fn link(cs: &mut [Container], parent: usize, child: usize) {
    cs[child].parent = Some(parent);
    cs[parent].children.push(child);
}

fn unlink(cs: &mut [Container], child: usize) {
    if let Some(p) = cs[child].parent.take() {
        cs[p].children.retain(|&c| c != child);
    }
}

/// Depth-first, children sorted by date. Empty containers emit no row and
/// their children take their depth. Iterative: thread depth is
/// attacker-controlled.
fn flatten(cs: &[Container], inputs: &[ThreadInput], earliest: &[i64], root: usize, out: &mut Vec<ThreadRow>) {
    let mut stack = vec![(root, 0u32)];
    while let Some((c, depth)) = stack.pop() {
        let child_depth = match cs[c].message {
            Some(i) => {
                out.push(ThreadRow { id: inputs[i].id, depth, thread_size: 0 });
                depth + 1
            }
            None => depth,
        };
        let mut children = cs[c].children.clone();
        children.sort_by_key(|&ch| earliest[ch]);
        // Reversed, so the earliest child is popped first.
        stack.extend(children.into_iter().rev().map(|ch| (ch, child_depth)));
    }
}

/// For every container, the date of its earliest message in the subtree
/// (`i64::MAX` if none). Iterative post-order.
fn earliest_dates(cs: &[Container], inputs: &[ThreadInput], roots: &[usize]) -> Vec<i64> {
    let mut earliest = vec![i64::MAX; cs.len()];
    let mut stack: Vec<(usize, bool)> = roots.iter().map(|&r| (r, false)).collect();
    while let Some((c, children_done)) = stack.pop() {
        if children_done {
            let own = cs[c].message.map(|i| inputs[i].date).unwrap_or(i64::MAX);
            earliest[c] = cs[c].children.iter().map(|&ch| earliest[ch]).fold(own, i64::min);
        } else {
            stack.push((c, true));
            stack.extend(cs[c].children.iter().map(|&ch| (ch, false)));
        }
    }
    earliest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: i64, date: i64, mid: &str, refs: &[&str]) -> ThreadInput {
        ThreadInput {
            id,
            date,
            subject: String::new(),
            message_id: Some(mid.into()),
            in_reply_to: refs.last().map(|s| s.to_string()),
            references: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn simple_thread_and_order() {
        let inputs = vec![
            m(1, 100, "a", &[]),
            m(2, 200, "b", &["a"]),
            m(3, 300, "c", &["a", "b"]),
            m(4, 250, "d", &["a"]),
            m(5, 150, "lonely", &[]),
        ];
        let rows = thread(&inputs);
        let order: Vec<(i64, u32)> = rows.iter().map(|r| (r.id, r.depth)).collect();
        // Thread "a" is newest (300) so first; children by date.
        assert_eq!(order, vec![(1, 0), (2, 1), (3, 2), (4, 1), (5, 0)]);
        assert_eq!(rows[0].thread_size, 4);
    }

    #[test]
    fn missing_parent_is_pruned() {
        // Two replies to a message we don't have: siblings under an empty root.
        let inputs = vec![m(1, 100, "x", &["gone"]), m(2, 200, "y", &["gone"])];
        let rows = thread(&inputs);
        assert_eq!(rows.iter().map(|r| (r.id, r.depth)).collect::<Vec<_>>(), vec![(1, 0), (2, 0)]);
    }

    #[test]
    fn deep_and_wide_references_are_safe() {
        // One message with a huge References list, and a 5 000 deep chain.
        let refs: Vec<String> = (0..100_000).map(|i| format!("r{i}")).collect();
        let mut inputs = vec![ThreadInput {
            id: 0,
            date: 0,
            subject: String::new(),
            message_id: Some("big".into()),
            in_reply_to: None,
            references: refs,
        }];
        for i in 1..5_000 {
            let parent = format!("m{}", i - 1);
            inputs.push(m(i, i, &format!("m{i}"), &[parent.as_str()]));
        }
        let rows = thread(&inputs);
        assert_eq!(rows.len(), 5_000);
        assert_eq!(rows.iter().map(|r| r.depth).max(), Some(4_998));
    }

    #[test]
    fn cycles_and_duplicates_dont_hang() {
        let inputs = vec![m(1, 1, "a", &["b"]), m(2, 2, "b", &["a"]), m(3, 3, "a", &[])];
        let rows = thread(&inputs);
        assert_eq!(rows.len(), 3);
    }
}
