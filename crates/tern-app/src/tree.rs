//! Folder tree view model (ARCHITECTURE.md §10): one root per account, real
//! IMAP hierarchy below it, no unified inbox.

use std::collections::HashMap;

use tern_core::model::{Folder, FolderRole};

use crate::types::FolderNode;

fn role_rank(role: Option<FolderRole>) -> u8 {
    match role {
        Some(FolderRole::Inbox) => 0,
        Some(FolderRole::Drafts) => 1,
        Some(FolderRole::Sent) => 2,
        Some(FolderRole::Archive) => 3,
        Some(FolderRole::Junk) => 4,
        Some(FolderRole::Trash) => 5,
        _ => 10,
    }
}

/// Append one account's subtree (pre-order) to `out`. `sent` is the
/// configured Sent folder (server name), shown with the `sent` role.
pub fn append_account(out: &mut Vec<FolderNode>, account: &str, name: &str, folders: &[Folder], sent: Option<&str>) {
    let folders: Vec<Folder> = folders
        .iter()
        .cloned()
        .map(|mut f| {
            if sent == Some(f.name.as_str()) {
                f.role = Some(FolderRole::Sent);
            }
            f
        })
        .collect();
    let folders = folders.as_slice();
    let root = out.len() as i32;
    out.push(FolderNode {
        account: account.into(),
        folder: 0,
        depth: 0,
        parent: -1,
        name: name.into(),
        path: String::new(),
        role: String::new(),
        selectable: false,
        unread: folders.iter().filter(|f| f.role == Some(FolderRole::Inbox)).map(|f| f.unread).sum(),
        total: 0,
    });

    let by_name: HashMap<&str, usize> = folders.iter().enumerate().map(|(i, f)| (f.name.as_str(), i)).collect();
    // Children per parent index (None = account root). A folder whose parent
    // isn't listed (e.g. "a/b" without "a") hangs off the root.
    let mut children: HashMap<Option<usize>, Vec<usize>> = HashMap::new();
    for (i, f) in folders.iter().enumerate() {
        let parent = f.parent_name().and_then(|p| by_name.get(p).copied());
        children.entry(parent).or_default().push(i);
    }
    for list in children.values_mut() {
        list.sort_by(|&a, &b| {
            let (fa, fb) = (&folders[a], &folders[b]);
            (role_rank(fa.role), fa.leaf_name().to_lowercase())
                .cmp(&(role_rank(fb.role), fb.leaf_name().to_lowercase()))
        });
    }

    fn walk(
        out: &mut Vec<FolderNode>,
        account: &str,
        folders: &[Folder],
        children: &HashMap<Option<usize>, Vec<usize>>,
        node: Option<usize>,
        parent_row: i32,
        depth: u32,
    ) {
        for &i in children.get(&node).map(Vec::as_slice).unwrap_or_default() {
            let f = &folders[i];
            let row = out.len() as i32;
            out.push(FolderNode {
                account: account.into(),
                folder: f.id,
                depth,
                parent: parent_row,
                name: f.leaf_name().into(),
                path: f.name.clone(),
                role: f.role.map(|r| r.as_str().to_owned()).unwrap_or_default(),
                selectable: f.selectable,
                unread: f.unread,
                total: f.total,
            });
            walk(out, account, folders, children, Some(i), row, depth + 1);
        }
    }
    walk(out, account, folders, &children, None, root, 1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: i64, name: &str, role: Option<FolderRole>) -> Folder {
        Folder {
            id,
            name: name.into(),
            delimiter: Some("/".into()),
            role,
            selectable: true,
            uidvalidity: None,
            uidnext: None,
            highestmodseq: None,
            total: 0,
            unread: id as u32,
        }
    }

    #[test]
    fn order_and_hierarchy() {
        let folders = vec![
            f(1, "Archive/2024", None),
            f(2, "Trash", Some(FolderRole::Trash)),
            f(3, "INBOX", Some(FolderRole::Inbox)),
            f(4, "Archive", Some(FolderRole::Archive)),
            f(5, "lists", None),
            f(6, "orphan/child", None),
        ];
        let mut out = Vec::new();
        append_account(&mut out, "acc", "Account", &folders, Some("lists"));
        let names: Vec<(&str, u32, i32)> = out.iter().map(|n| (n.name.as_str(), n.depth, n.parent)).collect();
        assert_eq!(
            names,
            vec![
                ("Account", 0, -1),
                ("INBOX", 1, 0),
                ("lists", 1, 0),
                ("Archive", 1, 0),
                ("2024", 2, 3),
                ("Trash", 1, 0),
                ("child", 1, 0),
            ]
        );
        assert_eq!(out[0].unread, 3);
        // The configured Sent folder ranks and shows as Sent.
        assert_eq!(out[2].role, "sent");
    }
}
