//! Pure functions that turn the host store into tree items.

use std::collections::HashSet;

use gpui_kit::SharedString;
use gpui_kit::component::tree::TreeItem;

use super::{GroupId, Host, HostGroup, HostId, HostNode};

/// Case-insensitive match on name, host or user.
pub fn matches_query(host: &Host, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    [&host.name, &host.address, &host.user]
        .iter()
        .any(|field| field.to_lowercase().contains(&query))
}

/// Case-insensitive match on a group's name.
fn group_matches_query(group: &HostGroup, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    !query.is_empty() && group.name.to_lowercase().contains(&query)
}

/// Build the nested tree. Each level lists its subgroups first, then the
/// hosts that belong to it in natural, case-insensitive name order; hosts
/// with no group sit at the root beside
/// the top-level groups. A non-empty query keeps the matching hosts and
/// the matching groups with everything in them, drops the groups left
/// without either, and expands what remains; otherwise `expanded` decides
/// which groups are open.
pub fn host_tree_items(
    groups: &[HostGroup],
    hosts: &[Host],
    query: &str,
    expanded: &HashSet<GroupId>,
) -> Vec<TreeItem> {
    let filtering = !query.trim().is_empty();
    let level = Level {
        groups,
        hosts,
        query,
        filtering,
        expanded,
    };
    items_under(&level, None, 0, false)
}

/// The first host the query itself matches, in the order the tree lists
/// them: what Enter in the search field connects. A host shown only because
/// its group matched does not count.
pub fn first_matching_host(groups: &[HostGroup], hosts: &[Host], query: &str) -> Option<HostId> {
    fn first(items: &[TreeItem], hosts: &[Host], query: &str) -> Option<HostId> {
        items.iter().find_map(|item| {
            match HostNode::parse(&item.id)? {
                HostNode::Host(id) => hosts
                    .iter()
                    .find(|host| host.id == id)
                    .filter(|host| matches_query(host, query))
                    .map(|host| host.id),
                HostNode::Group(_) => None,
            }
            .or_else(|| first(&item.children, hosts, query))
        })
    }
    let items = host_tree_items(groups, hosts, query, &HashSet::new());
    first(&items, hosts, query)
}

/// What every level of the tree is built from.
struct Level<'a> {
    groups: &'a [HostGroup],
    hosts: &'a [Host],
    query: &'a str,
    filtering: bool,
    expanded: &'a HashSet<GroupId>,
}

/// The groups directly under `parent`, in display order. `None` asks for the
/// top-level groups.
fn child_groups(groups: &[HostGroup], parent: Option<GroupId>) -> Vec<&HostGroup> {
    let mut children: Vec<_> = groups
        .iter()
        .filter(|group| group.parent == parent)
        .collect();
    children.sort_by_key(|group| (group.sort_order, group.id));
    children
}

/// The items under `parent`. `everything`: a group above matched the query,
/// so all of it is listed.
fn items_under(
    level: &Level,
    parent: Option<GroupId>,
    depth: usize,
    everything: bool,
) -> Vec<TreeItem> {
    // Nesting cannot run deeper than the number of groups unless the data has
    // a cycle, which the store refuses to create. Stop rather than recurse
    // forever if a hand-edited database ever produces one.
    if depth > level.groups.len() {
        return Vec::new();
    }
    let mut items: Vec<TreeItem> = Vec::new();
    for group in child_groups(level.groups, parent) {
        let matched = everything || group_matches_query(group, level.query);
        let children = items_under(level, Some(group.id), depth + 1, matched);
        if level.filtering && !matched && children.is_empty() {
            continue;
        }
        items.push(
            TreeItem::new(HostNode::Group(group.id).id(), group.name.clone())
                .expanded(level.filtering || level.expanded.contains(&group.id))
                .children(children),
        );
    }
    let mut child_hosts: Vec<_> = level
        .hosts
        .iter()
        .filter(|s| s.group == parent && (everything || matches_query(s, level.query)))
        .map(|host| (host, host.name.to_lowercase()))
        .collect();
    child_hosts.sort_by(|(a, a_name), (b, b_name)| {
        natural_name_order(a_name, b_name)
            .then_with(|| (a.sort_order, a.id).cmp(&(b.sort_order, b.id)))
    });
    items.extend(
        child_hosts
            .into_iter()
            .map(|(host, _)| TreeItem::new(HostNode::Host(host.id).id(), host.name.clone())),
    );
    items
}

/// Compare lowercase names without bounding the size of their numeric parts.
fn natural_name_order(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    while let (Some(left), Some(right)) = (a.first(), b.first()) {
        let order = if left.is_ascii_digit() && right.is_ascii_digit() {
            let a_end = a
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(a.len());
            let b_end = b
                .iter()
                .position(|c| !c.is_ascii_digit())
                .unwrap_or(b.len());
            let a_start = a[..a_end].iter().position(|c| *c != b'0').unwrap_or(a_end);
            let b_start = b[..b_end].iter().position(|c| *c != b'0').unwrap_or(b_end);
            let a_number = &a[a_start..a_end];
            let b_number = &b[b_start..b_end];
            let order = a_number
                .len()
                .cmp(&b_number.len())
                .then_with(|| a_number.cmp(b_number));
            a = &a[a_end..];
            b = &b[b_end..];
            order
        } else {
            let order = left.cmp(right);
            a = &a[1..];
            b = &b[1..];
            order
        };
        if !order.is_eq() {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

/// Groups flattened depth first, each labelled by its full path (e.g.
/// `生产 / 数据库`), for the group pickers in the forms. An excluded group is
/// skipped along with everything below it, which is how the rename dialog
/// keeps a group from being moved into its own subtree.
pub fn group_options(groups: &[HostGroup], excluded: &[GroupId]) -> Vec<(GroupId, SharedString)> {
    let mut options = Vec::new();
    collect_group_options(groups, None, "", 0, excluded, &mut options);
    options
}

fn collect_group_options(
    groups: &[HostGroup],
    parent: Option<GroupId>,
    prefix: &str,
    depth: usize,
    excluded: &[GroupId],
    options: &mut Vec<(GroupId, SharedString)>,
) {
    if depth > groups.len() {
        return;
    }
    for group in child_groups(groups, parent) {
        if excluded.contains(&group.id) {
            continue;
        }
        let path = if prefix.is_empty() {
            group.name.to_string()
        } else {
            format!("{prefix} / {}", group.name)
        };
        options.push((group.id, path.as_str().into()));
        collect_group_options(groups, Some(group.id), &path, depth + 1, excluded, options);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::{
        AuthKind, GroupDraft, HostData, HostDatabase, HostDraft, HostStore, NodeDrop,
        SerialSettings,
    };

    fn all_groups(store: &HostData) -> HashSet<GroupId> {
        store.groups().iter().map(|g| g.id).collect()
    }

    fn labels(items: &[TreeItem]) -> Vec<String> {
        items.iter().map(|item| item.label.to_string()).collect()
    }

    #[test]
    fn serial_hosts_sort_by_name_on_load_insert_rename_and_move() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.db");
        let mut store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let group = store.insert_group(GroupDraft::new("com", None));
        let child = store.insert_group(GroupDraft::new("nested", Some(group)));
        for name in ["COM3", "COM5", "COM6", "COM4", "COM10"] {
            store.insert(HostDraft::serial(
                name,
                name,
                SerialSettings::default(),
                Some(group),
            ));
        }
        let mut store = HostData::load(HostDatabase::open(&path).unwrap()).unwrap();
        let children = |store: &HostData| {
            host_tree_items(store.groups(), store.hosts(), "", &all_groups(store))[0]
                .children
                .clone()
        };
        assert_eq!(
            labels(&children(&store)),
            ["nested", "COM3", "COM4", "COM5", "COM6", "COM10"]
        );
        let id = store.insert(HostDraft::serial(
            "COM2",
            "COM2",
            SerialSettings::default(),
            Some(group),
        ));
        assert_eq!(
            labels(&children(&store)),
            ["nested", "COM2", "COM3", "COM4", "COM5", "COM6", "COM10"]
        );
        assert_eq!(
            first_matching_host(store.groups(), store.hosts(), "COM"),
            Some(id)
        );
        let mut draft = store.host(id).unwrap().draft();
        draft.name = "COM7".into();
        store.update(id, draft);
        assert_eq!(
            labels(&children(&store)),
            ["nested", "COM3", "COM4", "COM5", "COM6", "COM7", "COM10"]
        );
        store.move_node(HostNode::Host(id), NodeDrop::Into(child));
        assert_eq!(
            labels(&children(&store)),
            ["nested", "COM3", "COM4", "COM5", "COM6", "COM10"]
        );
        store.move_node(HostNode::Host(id), NodeDrop::Root);
        let items = host_tree_items(store.groups(), store.hosts(), "", &all_groups(&store));
        assert_eq!(labels(&items), ["com", "COM7"]);
    }

    #[test]
    fn natural_names_sort_case_insensitively_in_every_level() {
        let mut store = HostStore::empty();
        for name in [
            "机10",
            "机2",
            "rack10-port2",
            "rack2-port10",
            "rack2-port2",
            "com10",
            "COM3",
            "COM03",
        ] {
            store.insert_unnotified(HostDraft::new(
                name,
                "example.test",
                22,
                "root",
                AuthKind::NoPassword,
                None,
            ));
        }
        let items = host_tree_items(store.groups(), store.hosts(), "", &HashSet::new());
        assert_eq!(
            labels(&items),
            [
                "COM3",
                "COM03",
                "com10",
                "rack2-port2",
                "rack2-port10",
                "rack10-port2",
                "机2",
                "机10"
            ]
        );
        assert!(
            natural_name_order("com999999999999999999999", "com1000000000000000000000").is_lt()
        );
        assert!(natural_name_order("com00", "com0").is_eq());
        assert!(natural_name_order("com", "com0").is_lt());
    }

    #[test]
    fn node_id_round_trips() {
        for node in [HostNode::Group(GroupId(7)), HostNode::Host(HostId(42))] {
            assert_eq!(HostNode::parse(&node.id()), Some(node));
        }
        assert_eq!(HostNode::parse("x:1"), None);
        assert_eq!(HostNode::parse("s:abc"), None);
    }

    #[test]
    fn empty_query_keeps_every_group_and_host() {
        let store = HostStore::seed();
        let items = host_tree_items(store.groups(), store.hosts(), "", &all_groups(&store));
        assert_eq!(items.len(), 3);
        let total: usize = items.iter().map(|g| g.children.len()).sum();
        assert_eq!(total, 6);
        assert!(items.iter().all(|g| g.is_expanded()));
    }

    #[test]
    fn collapsed_groups_follow_the_expanded_set() {
        let store = HostStore::seed();
        let items = host_tree_items(store.groups(), store.hosts(), "", &HashSet::new());
        assert!(items.iter().all(|g| !g.is_expanded()));
    }

    #[test]
    fn query_matches_host_and_user_case_insensitively() {
        let store = HostStore::seed();
        let by_host = host_tree_items(store.groups(), store.hosts(), "10.0.9", &HashSet::new());
        let names: Vec<_> = by_host
            .iter()
            .flat_map(|g| g.children.iter().map(|c| c.label.to_string()))
            .collect();
        assert_eq!(names, ["qa-runner", "staging-api"]);

        let by_user = host_tree_items(store.groups(), store.hosts(), "POSTGRES", &HashSet::new());
        assert_eq!(by_user.len(), 1);
        assert_eq!(by_user[0].children[0].label.as_ref(), "db-01");
    }

    #[test]
    fn query_drops_groups_without_matches_and_expands_the_rest() {
        let store = HostStore::seed();
        let items = host_tree_items(store.groups(), store.hosts(), "dev", &HashSet::new());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label.as_ref(), "开发");
        assert!(items[0].is_expanded());
    }

    /// 生产 / 数据库 with one host each, plus a host at the root.
    fn nested_store() -> HostStore {
        let mut store = HostStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        for (name, group) in [
            ("web-01", Some(production)),
            ("db-01", Some(databases)),
            ("jump", None),
        ] {
            store.insert_unnotified(HostDraft::new(
                name,
                "10.0.0.1",
                22,
                "root",
                AuthKind::NoPassword,
                group,
            ));
        }
        store
    }

    #[test]
    fn subgroups_nest_and_ungrouped_hosts_sit_at_the_root() {
        let store = nested_store();
        let items = host_tree_items(store.groups(), store.hosts(), "", &all_groups(&store));
        // A top-level group, then the host that belongs to no group.
        assert_eq!(labels(&items), ["生产", "jump"]);
        // Inside 生产: the subgroup first, then its own host.
        assert_eq!(labels(&items[0].children), ["数据库", "web-01"]);
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);
    }

    #[test]
    fn a_match_deep_in_the_tree_keeps_its_whole_ancestor_chain() {
        let store = nested_store();
        let items = host_tree_items(store.groups(), store.hosts(), "db-01", &HashSet::new());
        assert_eq!(labels(&items), ["生产"]);
        assert!(items[0].is_expanded());
        assert_eq!(labels(&items[0].children), ["数据库"]);
        assert!(items[0].children[0].is_expanded());
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);
    }

    #[test]
    fn group_options_are_depth_first_and_skip_an_excluded_subtree() {
        let mut store = HostStore::empty();
        let production = store.insert_group_unnotified(GroupDraft::new("生产", None));
        let databases = store.insert_group_unnotified(GroupDraft::new("数据库", Some(production)));
        store.insert_group_unnotified(GroupDraft::new("只读副本", Some(databases)));
        store.insert_group_unnotified(GroupDraft::new("测试", None));

        let paths: Vec<_> = group_options(store.groups(), &[])
            .into_iter()
            .map(|(_, path)| path.to_string())
            .collect();
        assert_eq!(
            paths,
            ["生产", "生产 / 数据库", "生产 / 数据库 / 只读副本", "测试"]
        );

        // Renaming 数据库: neither it nor its children may become its parent.
        let mut excluded = store.descendant_groups(databases);
        excluded.push(databases);
        let paths: Vec<_> = group_options(store.groups(), &excluded)
            .into_iter()
            .map(|(_, path)| path.to_string())
            .collect();
        assert_eq!(paths, ["生产", "测试"]);
    }

    #[test]
    fn a_query_matching_a_group_lists_all_of_it() {
        let store = nested_store();
        let items = host_tree_items(store.groups(), store.hosts(), "生产", &HashSet::new());
        assert_eq!(labels(&items), ["生产"]);
        assert!(items[0].is_expanded());
        assert_eq!(labels(&items[0].children), ["数据库", "web-01"]);
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);

        // A subgroup: its ancestors for the way there, but not their hosts.
        let items = host_tree_items(store.groups(), store.hosts(), "数据库", &HashSet::new());
        assert_eq!(labels(&items), ["生产"]);
        assert_eq!(labels(&items[0].children), ["数据库"]);
        assert_eq!(labels(&items[0].children[0].children), ["db-01"]);
    }

    #[test]
    fn enter_connects_the_first_host_the_query_matches_as_the_tree_lists_them() {
        let store = nested_store();
        let id = |name: &str| store.hosts().iter().find(|h| h.name == name).unwrap().id;
        // In the tree, 数据库's db-01 comes before 生产's own web-01.
        assert_eq!(
            first_matching_host(store.groups(), store.hosts(), "-01"),
            Some(id("db-01"))
        );
        // Hosts listed only because their group matched are not chosen.
        assert_eq!(
            first_matching_host(store.groups(), store.hosts(), "生产"),
            None
        );
    }

    #[test]
    fn a_query_matching_only_a_root_host_drops_every_group() {
        let store = nested_store();
        let items = host_tree_items(store.groups(), store.hosts(), "jump", &HashSet::new());
        assert_eq!(labels(&items), ["jump"]);
    }
}
