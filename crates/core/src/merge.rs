//! Three-way merges for memo payloads and the vault. See `docs/sync.md#merge-rules`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use uuid::Uuid;

use crate::payload::{AttachmentRef, Contact, MEMO_PAYLOAD_VERSION, MemoPayload, VaultPayload};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoMerge {
    Clean(MemoPayload),
    /// The bodies have overlapping edits. Nothing is merged; the user decides.
    Conflict,
}

/// Merges `ours` and `theirs`, which both descend from `base`. All three must be editable payloads.
pub fn merge_memo(base: &MemoPayload, ours: &MemoPayload, theirs: &MemoPayload) -> MemoMerge {
    if ours == base || ours == theirs {
        return MemoMerge::Clean(theirs.clone());
    }
    if theirs == base {
        return MemoMerge::Clean(ours.clone());
    }
    let body = if ours.body == base.body {
        theirs.body.clone()
    } else if theirs.body == base.body || ours.body == theirs.body {
        ours.body.clone()
    } else {
        match diffy::merge(&base.body, &ours.body, &theirs.body) {
            Ok(body) => body,
            Err(_) => return MemoMerge::Conflict,
        }
    };
    MemoMerge::Clean(MemoPayload {
        v: MEMO_PAYLOAD_VERSION,
        body,
        edited_at: ours.edited_at.max(theirs.edited_at),
        attachments: merge_attachments(&base.attachments, &ours.attachments, &theirs.attachments),
    })
}

/// Kept if either side has it and neither side removed it; theirs wins when both changed it.
/// Order: theirs, then attachments only ours added.
fn merge_attachments(base: &[AttachmentRef], ours: &[AttachmentRef], theirs: &[AttachmentRef]) -> Vec<AttachmentRef> {
    let index = |list: &[AttachmentRef]| list.iter().map(|a| (a.id, a.clone())).collect::<BTreeMap<_, _>>();
    let (base_map, ours_map, theirs_map) = (index(base), index(ours), index(theirs));
    let pick = |id: &Uuid| -> Option<AttachmentRef> {
        match (base_map.get(id), ours_map.get(id), theirs_map.get(id)) {
            (Some(_), None, _) | (Some(_), _, None) => None,
            (Some(b), Some(o), Some(t)) => Some(if t == b { o.clone() } else { t.clone() }),
            (None, _, Some(t)) => Some(t.clone()),
            (None, Some(o), None) => Some(o.clone()),
            (None, None, None) => None,
        }
    };
    let mut seen = BTreeSet::new();
    theirs.iter().chain(ours).filter(|a| seen.insert(a.id)).filter_map(|a| pick(&a.id)).collect()
}

/// Both sides pinned a different public key for the same user: one of them came from a lying
/// server. The merged vault keeps the older pin; the UI must raise the key-change alarm.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PinConflict {
    pub user_id: Uuid,
    pub kept: Contact,
    pub dropped: Contact,
}

/// Contacts are a union. Settings merge per key; theirs wins when both sides changed a key. Shares
/// are a three-way set merge per memo.
pub fn merge_vault(
    base: &VaultPayload,
    ours: &VaultPayload,
    theirs: &VaultPayload,
) -> (VaultPayload, Vec<PinConflict>) {
    let mut conflicts = Vec::new();
    let mut contacts = theirs.contacts.clone();
    for (id, mine) in &ours.contacts {
        let Some(other) = contacts.get(id) else {
            contacts.insert(*id, mine.clone());
            continue;
        };
        if mine.public_key == other.public_key {
            let merged = Contact {
                username: other.username.clone(),
                public_key: other.public_key,
                pinned_at: mine.pinned_at.min(other.pinned_at),
                verified: mine.verified || other.verified,
            };
            contacts.insert(*id, merged);
        } else {
            let (kept, dropped) = if mine.pinned_at < other.pinned_at {
                (mine.clone(), other.clone())
            } else {
                (other.clone(), mine.clone())
            };
            contacts.insert(*id, kept.clone());
            conflicts.push(PinConflict { user_id: *id, kept, dropped });
        }
    }
    let settings = merge_settings(&base.settings, &ours.settings, &theirs.settings);
    let shares = merge_shares(&base.shares, &ours.shares, &theirs.shares);
    (VaultPayload { v: theirs.v.max(ours.v), contacts, settings, shares }, conflicts)
}

type Shares = BTreeMap<Uuid, BTreeSet<Uuid>>;

/// A user stays if both sides have them or one side added them, and goes if one side removed them.
fn merge_shares(base: &Shares, ours: &Shares, theirs: &Shares) -> Shares {
    let empty = BTreeSet::new();
    let memos: BTreeSet<&Uuid> = ours.keys().chain(theirs.keys()).collect();
    memos
        .into_iter()
        .filter_map(|memo| {
            let (b, o, t) = (
                base.get(memo).unwrap_or(&empty),
                ours.get(memo).unwrap_or(&empty),
                theirs.get(memo).unwrap_or(&empty),
            );
            let users: BTreeSet<Uuid> =
                o.union(t).filter(|u| (o.contains(u) && t.contains(u)) || !b.contains(u)).copied().collect();
            (!users.is_empty()).then_some((*memo, users))
        })
        .collect()
}

fn merge_settings(
    base: &Map<String, Value>,
    ours: &Map<String, Value>,
    theirs: &Map<String, Value>,
) -> Map<String, Value> {
    let keys: BTreeSet<&String> = base.keys().chain(ours.keys()).chain(theirs.keys()).collect();
    keys.into_iter()
        .filter_map(|key| {
            let (b, o, t) = (base.get(key), ours.get(key), theirs.get(key));
            let value = if o == b {
                t
            } else if t == b {
                o
            } else {
                t
            };
            value.map(|v| (key.clone(), v.clone()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{AttachmentKey, PublicKey};
    use serde_json::json;

    #[test]
    fn vault_shares_merge_as_sets() {
        let id = |n: u8| Uuid::from_bytes([n; 16]);
        let vault = |shares: &[(u8, &[u8])]| VaultPayload {
            shares: shares.iter().map(|(memo, users)| (id(*memo), users.iter().map(|u| id(*u)).collect())).collect(),
            ..Default::default()
        };
        let base = vault(&[(1, &[10, 11]), (2, &[20])]);
        // Ours adds 12 to memo 1 and shares memo 3; theirs removes 11 and stops sharing memo 2.
        let ours = vault(&[(1, &[10, 11, 12]), (2, &[20]), (3, &[30])]);
        let theirs = vault(&[(1, &[10])]);
        let (merged, conflicts) = merge_vault(&base, &ours, &theirs);
        assert!(conflicts.is_empty());
        assert_eq!(merged.shares, vault(&[(1, &[10, 12]), (3, &[30])]).shares);
        // Rebased on an older vault (a reset after the server lost data), ours stands.
        assert_eq!(merge_vault(&base, &theirs, &base).0.shares, theirs.shares);
    }

    fn memo(body: &str, edited_at: i64) -> MemoPayload {
        MemoPayload::new(body, edited_at)
    }

    fn attachment(n: u8, mime: &str) -> AttachmentRef {
        AttachmentRef {
            id: Uuid::from_bytes([n; 16]),
            key: AttachmentKey::from_bytes(&[n; 32]).unwrap(),
            mime: mime.into(),
            size: 10,
            width: 1,
            height: 1,
            thumb: None,
        }
    }

    #[test]
    fn non_overlapping_body_edits_merge() {
        let base = memo("a\nb\nc\n", 1);
        let ours = memo("A\nb\nc\n", 3);
        let theirs = memo("a\nb\nC\n", 2);
        assert_eq!(merge_memo(&base, &ours, &theirs), MemoMerge::Clean(memo("A\nb\nC\n", 3)));
    }

    #[test]
    fn overlapping_body_edits_conflict() {
        let base = memo("a\nb\n", 1);
        assert_eq!(merge_memo(&base, &memo("x\nb\n", 2), &memo("y\nb\n", 3)), MemoMerge::Conflict);
    }

    #[test]
    fn identical_edits_and_one_sided_edits_are_clean() {
        let base = memo("a", 1);
        let edit = memo("b", 2);
        assert_eq!(merge_memo(&base, &edit, &edit), MemoMerge::Clean(edit.clone()));
        assert_eq!(merge_memo(&base, &base, &edit), MemoMerge::Clean(edit.clone()));
        assert_eq!(merge_memo(&base, &edit, &base), MemoMerge::Clean(edit.clone()));
        // Same body on both sides, different timestamps: still no conflict.
        assert_eq!(merge_memo(&base, &memo("b", 5), &memo("b", 4)), MemoMerge::Clean(memo("b", 5)));
    }

    #[test]
    fn attachment_set_merge() {
        let (one, two, three, four) = (attachment(1, "a"), attachment(2, "a"), attachment(3, "a"), attachment(4, "a"));
        let mut base = memo("x", 1);
        base.attachments = vec![one.clone(), two.clone(), three.clone()];
        let mut ours = memo("x", 2);
        // ours: removes 2, changes 3, adds 4.
        ours.attachments = vec![one.clone(), attachment(3, "ours"), four.clone()];
        let mut theirs = memo("x", 3);
        // theirs: removes 1, changes 3 too; keeps 2.
        theirs.attachments = vec![two.clone(), attachment(3, "theirs")];
        let MemoMerge::Clean(merged) = merge_memo(&base, &ours, &theirs) else { panic!() };
        assert_eq!(merged.attachments, vec![attachment(3, "theirs"), four]);
    }

    fn contact(key: u8, pinned_at: i64, verified: bool) -> Contact {
        Contact { username: "bob".into(), public_key: PublicKey::from_bytes(&[key; 32]).unwrap(), pinned_at, verified }
    }

    #[test]
    fn vault_contacts_union_and_pin_conflicts() {
        let (bob, carol, dave) = (Uuid::from_bytes([1; 16]), Uuid::from_bytes([2; 16]), Uuid::from_bytes([3; 16]));
        let base = VaultPayload::default();
        let mut ours = VaultPayload::default();
        ours.contacts.insert(bob, contact(1, 10, true));
        ours.contacts.insert(carol, contact(2, 10, false));
        ours.contacts.insert(dave, contact(4, 20, false));
        let mut theirs = VaultPayload::default();
        theirs.contacts.insert(bob, contact(1, 5, false));
        theirs.contacts.insert(dave, contact(5, 30, true));

        let (merged, conflicts) = merge_vault(&base, &ours, &theirs);
        assert_eq!(merged.contacts[&bob], contact(1, 5, true));
        assert_eq!(merged.contacts[&carol], contact(2, 10, false));
        assert_eq!(merged.contacts[&dave], contact(4, 20, false));
        assert_eq!(
            conflicts,
            vec![PinConflict { user_id: dave, kept: contact(4, 20, false), dropped: contact(5, 30, true) }]
        );
    }

    #[test]
    fn vault_settings_merge_per_key() {
        let settings =
            |v: serde_json::Value| VaultPayload { settings: v.as_object().unwrap().clone(), ..Default::default() };
        let base = settings(json!({ "theme": "dark", "font": 12, "gone": 1 }));
        let ours = settings(json!({ "theme": "light", "font": 12, "gone": 1, "new": true }));
        let theirs = settings(json!({ "theme": "dark", "font": 14 }));
        let (merged, _) = merge_vault(&base, &ours, &theirs);
        assert_eq!(merged.settings, settings(json!({ "theme": "light", "font": 14, "new": true })).settings);
    }
}
