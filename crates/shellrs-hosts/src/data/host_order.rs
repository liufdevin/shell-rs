use std::cmp::Ordering;

use super::{Change, GroupId, Host, HostData};

impl HostData {
    pub fn hosts_manually_ordered(&self, parent: Option<GroupId>) -> bool {
        self.manual_host_order.contains(&parent)
    }

    pub fn hosts_in_order(&self, parent: Option<GroupId>) -> Vec<&Host> {
        let mut hosts: Vec<_> = self
            .hosts
            .iter()
            .filter(|host| host.group == parent)
            .map(|host| (host, host.name.to_lowercase()))
            .collect();
        let manual = self.hosts_manually_ordered(parent);
        hosts.sort_by(|(left, left_name), (right, right_name)| {
            let name_order = if manual {
                Ordering::Equal
            } else {
                natural_name_order(left_name, right_name)
            };
            name_order.then_with(|| (left.sort_order, left.id).cmp(&(right.sort_order, right.id)))
        });
        hosts.into_iter().map(|(host, _)| host).collect()
    }

    pub fn automatically_order_hosts(&mut self, parent: Option<GroupId>) -> bool {
        if !self.manual_host_order.remove(&parent) {
            return false;
        }
        self.persist(Change::HostOrder, |db| {
            db.save_tree_order(&self.groups, &self.hosts, &self.manual_host_order)
        });
        self.changed();
        true
    }
}

fn natural_name_order(left: &str, right: &str) -> Ordering {
    let (mut left, mut right) = (left.as_bytes(), right.as_bytes());
    while let (Some(left_first), Some(right_first)) = (left.first(), right.first()) {
        let order = if left_first.is_ascii_digit() && right_first.is_ascii_digit() {
            let left_end = left
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .unwrap_or(left.len());
            let right_end = right
                .iter()
                .position(|byte| !byte.is_ascii_digit())
                .unwrap_or(right.len());
            let left_start = left[..left_end]
                .iter()
                .position(|byte| *byte != b'0')
                .unwrap_or(left_end);
            let right_start = right[..right_end]
                .iter()
                .position(|byte| *byte != b'0')
                .unwrap_or(right_end);
            let left_number = &left[left_start..left_end];
            let right_number = &right[right_start..right_end];
            let order = left_number
                .len()
                .cmp(&right_number.len())
                .then_with(|| left_number.cmp(right_number));
            left = &left[left_end..];
            right = &right[right_end..];
            order
        } else {
            let order = left_first.cmp(right_first);
            left = &left[1..];
            right = &right[1..];
            order
        };
        if !order.is_eq() {
            return order;
        }
    }
    left.len().cmp(&right.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GroupDraft, HostDatabase, HostDraft, HostNode, NodeDrop, SerialSettings};

    fn insert(store: &mut HostData, name: &str, parent: Option<GroupId>) -> crate::HostId {
        store.insert(HostDraft::serial(
            name,
            name,
            SerialSettings::default(),
            parent,
        ))
    }

    fn names(store: &HostData, parent: Option<GroupId>) -> Vec<String> {
        store
            .hosts_in_order(parent)
            .iter()
            .map(|host| host.name.to_string())
            .collect()
    }

    #[test]
    fn manual_host_order_survives_reload_edits_and_automatic_reset() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts.db");
        let mut store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let group = store.insert_group(GroupDraft::new("com", None));
        let parent = Some(group);
        let first = insert(&mut store, "COM3", parent);
        insert(&mut store, "COM5", parent);
        let fourth = insert(&mut store, "COM4", parent);
        insert(&mut store, "COM10", parent);
        insert(&mut store, "COM2", None);
        assert_eq!(names(&store, parent), ["COM3", "COM4", "COM5", "COM10"]);
        assert!(!store.move_node(
            HostNode::Host(first),
            NodeDrop::Before(HostNode::Host(first))
        ));
        assert!(!store.hosts_manually_ordered(parent));
        assert!(store.move_node(
            HostNode::Host(first),
            NodeDrop::After(HostNode::Host(fourth))
        ));
        assert_eq!(names(&store, parent), ["COM4", "COM3", "COM5", "COM10"]);
        drop(store);
        let mut store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        assert!(store.hosts_manually_ordered(parent));
        assert!(!store.hosts_manually_ordered(None));
        assert_eq!(names(&store, parent), ["COM4", "COM3", "COM5", "COM10"]);
        let mut draft = store.host(first).unwrap().draft();
        draft.name = "COM1".into();
        store.update(first, draft);
        insert(&mut store, "COM0", parent);
        let moved = insert(&mut store, "COM6", None);
        assert!(store.move_node(HostNode::Host(moved), NodeDrop::Into(group)));
        assert_eq!(
            names(&store, parent),
            ["COM4", "COM1", "COM5", "COM10", "COM0", "COM6"]
        );
        assert!(store.automatically_order_hosts(parent));
        assert!(!store.automatically_order_hosts(parent));
        drop(store);
        let store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        assert!(!store.hosts_manually_ordered(parent));
        assert_eq!(
            names(&store, parent),
            ["COM0", "COM1", "COM4", "COM5", "COM6", "COM10"]
        );
    }

    #[test]
    fn root_and_nested_groups_keep_independent_order_modes_and_deletions_cascade() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts.db");
        let mut store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let group = store.insert_group(GroupDraft::new("com", None));
        let nested = store.insert_group(GroupDraft::new("nested", Some(group)));
        for parent in [None, Some(group), Some(nested)] {
            let first = insert(&mut store, "COM1", parent);
            let second = insert(&mut store, "COM2", parent);
            assert!(store.move_node(
                HostNode::Host(second),
                NodeDrop::Before(HostNode::Host(first))
            ));
        }
        assert!(store.automatically_order_hosts(Some(group)));
        assert!(store.hosts_manually_ordered(None));
        assert!(store.hosts_manually_ordered(Some(nested)));
        store.remove_group(group);
        assert!(!store.hosts_manually_ordered(Some(nested)));
        drop(store);
        let store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        assert!(store.hosts_manually_ordered(None));
        assert!(!store.hosts_manually_ordered(Some(nested)));
        assert_eq!(names(&store, None), ["COM2", "COM1"]);
    }

    #[test]
    fn natural_names_compare_unbounded_numbers_and_leading_zeroes() {
        assert!(
            natural_name_order("com999999999999999999999", "com1000000000000000000000").is_lt()
        );
        assert!(natural_name_order("com00", "com0").is_eq());
        assert!(natural_name_order("com", "com0").is_lt());
    }
}
