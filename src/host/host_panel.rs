use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, VirtualListScrollHandle,
    button::{Button, ButtonVariants as _},
    dock::{BasePanel, Panel, PanelControl, PanelEvent},
    h_flex,
    input::{Input, InputEvent, InputState},
    list::ListItem,
    menu::{ContextMenuExt as _, PopupMenu},
    scroll::ScrollableElement as _,
    tree::TreeItem,
    v_flex, v_virtual_list,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::app::{
    AutomaticallyOrderHosts, CatalogIcon, CollapseAllGroups, CollapseSelectedGroup, ConnectGroup,
    ConnectHost, ConnectSelected, CopyHostAddress, CopyHostId, DeleteGroup, DeleteHost,
    DuplicateHost, EditHost, ExpandAllGroups, ExpandSelectedGroup, HOST_PANEL_CONTEXT,
    HOST_TREE_CONTEXT, ImportKnownHosts, ImportSshConfig, MoveHostNode, NewChildGroup, NewGroup,
    NewHost, NewHostInGroup, OpenExplorer, RenameGroup, SelectNextHostRow, SelectPreviousHostRow,
    ToggleHostDetails,
};

use crate::i18n::{UiLocale, t, tn};
use crate::shared::{HostMark, RowTooltip, RowTooltips};

use super::{
    GroupId, Host, HostId, HostNode, HostOs, HostStore, NodeDrop, ProtocolKind,
    first_matching_host, host_tree_items,
};

/// The host list of the left dock: a searchable, grouped tree of hosts.
/// The workspace's sidebar shows it, and borrows its title and toolbar for
/// the dock's title bar while it does.
///
/// Owns the tree and search state; the host data lives in the shared
/// `HostStore`, which this panel observes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShowHostDetails(pub bool);

impl Global for ShowHostDetails {}

impl ShowHostDetails {
    pub fn get(cx: &App) -> bool {
        cx.try_global::<Self>().is_some_and(|details| details.0)
    }
}

#[derive(Clone)]
struct TreeRow {
    node: HostNode,
    label: SharedString,
    depth: usize,
    expanded: bool,
}

pub struct HostPanel {
    store: Entity<HostStore>,
    rows: Rc<Vec<TreeRow>>,
    selected: Option<HostNode>,
    details: bool,
    folded_in_search: HashSet<GroupId>,
    scroll_handle: VirtualListScrollHandle,
    tree_focus: FocusHandle,
    search: Entity<InputState>,
    query: String,
    expanded: HashSet<GroupId>,
    /// What the store held the last time the tree was rebuilt. Comparing
    /// against these is how a group or host created in a dialog gets
    /// revealed without the dialog having to report back to the panel.
    known_groups: HashSet<GroupId>,
    seen_hosts: HashSet<HostId>,
    /// Which node the last right-click landed on, `None` for the blank space
    /// below the rows. Shared with the row renderer and the context menu
    /// builder, both of which run outside this entity.
    right_clicked: Rc<Cell<Option<HostNode>>>,
    drop_target: Rc<Cell<Option<(HostNode, NodeDrop)>>>,
    /// Host rows' tooltips: where the host logs in, and its notes.
    row_tooltips: RowTooltips<HostId>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl HostPanel {
    pub fn new(store: Entity<HostStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (expanded, known_groups, seen_hosts) = {
            let read = store.read(cx);
            let expanded: HashSet<GroupId> = read
                .groups()
                .iter()
                .filter(|group| group.expanded)
                .map(|group| group.id)
                .collect();
            (
                expanded,
                read.groups().iter().map(|group| group.id).collect(),
                read.hosts().iter().map(|s| s.id).collect(),
            )
        };
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(t!("host.panel.search"))
                .clean_on_escape()
        });

        let subscriptions = vec![
            cx.observe(&store, |this, _, cx| this.on_store_changed(cx)),
            cx.observe_global::<ShowHostDetails>(|this, cx| {
                this.details = ShowHostDetails::get(cx);
                if let Some(index) = this
                    .selected
                    .and_then(|node| this.rows.iter().position(|row| row.node == node))
                {
                    this.scroll_handle
                        .scroll_to_item(index, ScrollStrategy::Nearest);
                }
                cx.notify();
            }),
            cx.observe_global_in::<UiLocale>(window, |this, window, cx| {
                this.search.update(cx, |search, cx| {
                    search.set_placeholder(t!("host.panel.search"), window, cx)
                });
            }),
            cx.subscribe_in(
                &search,
                window,
                |this, state, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => {
                        this.query = state.read(cx).value().to_string();
                        this.folded_in_search.clear();
                        this.rebuild_tree(cx);
                    }
                    InputEvent::PressEnter { .. } => this.connect_first_match(window, cx),
                    _ => {}
                },
            ),
        ];

        let mut panel = Self {
            store,
            rows: Rc::default(),
            selected: None,
            details: ShowHostDetails::get(cx),
            folded_in_search: HashSet::new(),
            scroll_handle: VirtualListScrollHandle::new(),
            tree_focus: cx.focus_handle(),
            search,
            query: String::new(),
            expanded,
            known_groups,
            seen_hosts,
            right_clicked: Rc::new(Cell::new(None)),
            drop_target: Rc::new(Cell::new(None)),
            row_tooltips: RowTooltips::new("host-tooltip", cx),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        };
        panel.rebuild_tree(cx);
        panel
    }

    /// Rebuild the tree, then put the cursor on whatever the store just
    /// gained so a freshly created group or host is visible and selected.
    fn on_store_changed(&mut self, cx: &mut Context<Self>) {
        let created = self.take_created_node(cx);
        self.rebuild_tree(cx);
        if let Some(node) = created {
            self.select_node(node, cx);
        }
    }

    /// The node the store gained since the last rebuild, if any, with the
    /// folders above it opened so it can be scrolled to.
    fn take_created_node(&mut self, cx: &mut Context<Self>) -> Option<HostNode> {
        let (created, ancestors, groups, hosts) = {
            let store = self.store.read(cx);
            let created = store
                .groups()
                .iter()
                .rev()
                .find(|group| !self.known_groups.contains(&group.id))
                .map(|group| HostNode::Group(group.id))
                .or_else(|| {
                    store
                        .hosts()
                        .iter()
                        .rev()
                        .find(|host| !self.seen_hosts.contains(&host.id))
                        .map(|host| HostNode::Host(host.id))
                });
            let ancestors = created
                .map(|node| groups_above(store, node))
                .unwrap_or_default();
            (
                created,
                ancestors,
                store.groups().iter().map(|g| g.id).collect(),
                store.hosts().iter().map(|s| s.id).collect(),
            )
        };
        self.known_groups = groups;
        self.seen_hosts = hosts;
        self.expanded.extend(ancestors);
        if let Some(HostNode::Group(id)) = created
            && self
                .store
                .read(cx)
                .group(id)
                .is_some_and(|group| group.expanded)
        {
            self.expanded.insert(id);
        }
        created
    }

    /// Rebuild the tree from the store, keeping the selection by id.
    fn rebuild_tree(&mut self, cx: &mut Context<Self>) {
        let items = {
            let store = self.store.read(cx);
            host_tree_items(store, &self.query, &self.expanded)
        };
        let mut rows = Vec::new();
        flatten(&items, 0, &self.folded_in_search, &mut rows);
        if self
            .selected
            .is_some_and(|node| !rows.iter().any(|row| row.node == node))
        {
            self.selected = None;
        }
        self.rows = Rc::new(rows);
        cx.notify();
    }

    /// Select (and reveal) a row, for example a fresh duplicate or a group
    /// just created.
    pub fn select_node(&mut self, node: HostNode, cx: &mut Context<Self>) {
        self.rebuild_tree(cx);
        self.selected = self.rows.iter().any(|row| row.node == node).then_some(node);
        if let Some(index) = self.rows.iter().position(|row| row.node == node) {
            self.scroll_handle
                .scroll_to_item(index, ScrollStrategy::Center);
        }
    }

    /// Keep the moved row visible even when it was dropped into a closed group.
    pub fn reveal_node(&mut self, node: HostNode, cx: &mut Context<Self>) {
        let ancestors = groups_above(self.store.read(cx), node);
        self.expanded.extend(ancestors);
        self.select_node(node, cx);
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |input, cx| input.focus(window, cx));
    }

    /// Refresh the visible tree after a bulk expansion change in the store.
    pub fn set_all_groups_expanded(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.folded_in_search.clear();
        self.expanded = if expanded {
            self.store
                .read(cx)
                .groups()
                .iter()
                .map(|group| group.id)
                .collect()
        } else {
            HashSet::new()
        };
        self.right_clicked.set(None);
        self.rebuild_tree(cx);
    }

    /// The node currently selected in the tree.
    pub fn selected_node(&self, _: &App) -> Option<HostNode> {
        self.selected
    }

    fn connect_first_match(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let store = self.store.read(cx);
        let first = first_matching_host(store, &self.query);
        if let Some(id) = first {
            window.dispatch_action(Box::new(ConnectHost(id)), cx);
        }
    }

    fn on_connect_selected(
        &mut self,
        _: &ConnectSelected,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_node(cx) {
            Some(HostNode::Host(id)) => {
                window.dispatch_action(Box::new(ConnectHost(id)), cx);
            }
            Some(HostNode::Group(group)) => {
                self.toggle_group(group, cx);
            }
            None => {}
        }
    }

    fn press_row(&mut self, node: HostNode, cx: &mut Context<Self>) {
        self.selected = Some(node);
        if let HostNode::Group(group) = node {
            self.toggle_group(group, cx);
        } else {
            cx.notify();
        }
    }

    fn toggle_group(&mut self, group: GroupId, cx: &mut Context<Self>) {
        if !self.query.trim().is_empty() {
            if !self.folded_in_search.remove(&group) {
                self.folded_in_search.insert(group);
            }
        } else {
            let expanded = !self.expanded.remove(&group);
            if expanded {
                self.expanded.insert(group);
            }
            self.store.update(cx, |store, cx| {
                store.set_group_expanded(group, expanded, cx)
            });
        }
        self.rebuild_tree(cx);
    }

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        self.right_clicked.set(None);
        let Some(last) = self.rows.len().checked_sub(1) else {
            return;
        };
        let current = self
            .selected
            .and_then(|node| self.rows.iter().position(|row| row.node == node));
        let next = match (current, down) {
            (Some(index), true) if index < last => index + 1,
            (_, true) => 0,
            (Some(index), false) => index.checked_sub(1).unwrap_or(last),
            (None, false) => last,
        };
        self.selected = Some(self.rows[next].node);
        self.scroll_handle
            .scroll_to_item(next, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn fold_selected(&mut self, expanded: bool, cx: &mut Context<Self>) {
        self.right_clicked.set(None);
        let Some(HostNode::Group(group)) = self.selected else {
            return;
        };
        if self
            .rows
            .iter()
            .find(|row| row.node == HostNode::Group(group))
            .is_some_and(|row| row.expanded != expanded)
        {
            self.toggle_group(group, cx);
        }
    }
}

fn flatten(items: &[TreeItem], depth: usize, folded: &HashSet<GroupId>, rows: &mut Vec<TreeRow>) {
    for item in items {
        let Some(node) = HostNode::parse(&item.id) else {
            continue;
        };
        let expanded =
            item.is_expanded() && node.group_id().is_none_or(|group| !folded.contains(&group));
        rows.push(TreeRow {
            node,
            label: item.label.clone(),
            depth,
            expanded,
        });
        if expanded {
            flatten(&item.children, depth + 1, folded, rows);
        }
    }
}

/// Every group a node sits in, at any depth: the folders that have to be
/// open for its row to show.
fn groups_above(store: &HostStore, node: HostNode) -> Vec<GroupId> {
    let parent = match node {
        HostNode::Group(id) => store.group(id).and_then(|group| group.parent),
        HostNode::Host(id) => store.host(id).and_then(|host| host.group),
    };
    parent
        .map(|id| {
            let mut chain = store.ancestor_groups(id);
            chain.push(id);
            chain
        })
        .unwrap_or_default()
}

impl EventEmitter<PanelEvent> for HostPanel {}

impl Focusable for HostPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BasePanel for HostPanel {
    fn panel_name(&self) -> &'static str {
        "HostPanel"
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn zoomable(&self, _: &App) -> bool {
        false
    }
}

impl Panel for HostPanel {
    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .gap_1()
            .child(Icon::new(CatalogIcon::Server).small())
            .child(t!("host.panel.title"))
    }

    fn toolbar_buttons(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<Vec<Button>> {
        // A new host first: it is what the list is mostly added to.
        Some(vec![
            Button::new("new-host-panel")
                .icon(IconName::Plus)
                .tooltip(t!("host.menu.new_host"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NewHost), cx)),
            Button::new("new-group")
                .icon(Icon::new(CatalogIcon::FolderPlus))
                .tooltip(t!("host.menu.new_group"))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(NewGroup), cx)),
        ])
    }

    fn zoom_control(&self, _: &App) -> Option<PanelControl> {
        None
    }

    fn inner_padding(&self, _: &App) -> bool {
        false
    }
}

impl Render for HostPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Plain snapshots for the row renderer: render callbacks must not
        // read entities.
        let host_marks: Rc<HashMap<HostId, MarkOf>> = Rc::new(
            self.store
                .read(cx)
                .hosts()
                .iter()
                .map(|host| {
                    let mark = MarkOf {
                        os: host.os,
                        protocol: host.kind(),
                        address: host.address.clone(),
                        details: if self.details {
                            std::iter::once(location(host))
                                .chain(note_line(&host.notes))
                                .collect()
                        } else {
                            Vec::new()
                        },
                    };
                    (host.id, mark)
                })
                .collect(),
        );
        let tooltips: Rc<HashMap<HostId, RowTooltip>> = Rc::new(
            self.store
                .read(cx)
                .hosts()
                .iter()
                .map(|host| {
                    let tooltip = RowTooltip::new(host.endpoint()).note(host.notes.clone());
                    (host.id, tooltip)
                })
                .collect(),
        );
        let group_parents: Rc<HashMap<GroupId, Option<GroupId>>> = Rc::new(
            self.store
                .read(cx)
                .groups()
                .iter()
                .map(|group| (group.id, group.parent))
                .collect(),
        );
        let group_counts = Rc::new(group_host_counts(
            self.store.read(cx).hosts(),
            &group_parents,
        ));
        let clicked_row = self.right_clicked.clone();
        let clicked_blank = self.right_clicked.clone();
        let clicked_menu = self.right_clicked.clone();
        let drop_target = self.drop_target.clone();
        let drop_target_for_root = self.drop_target.clone();
        let drop_target_for_move = self.drop_target.clone();
        let can_reorder = self.query.trim().is_empty();
        let row_interactions = Rc::new(RowInteractions {
            group_parents,
            right_clicked: clicked_row,
            drop_target,
            can_reorder,
            tooltips,
            row_tooltips: self.row_tooltips.clone(),
            details: self.details,
        });
        let heights = RowHeights::measure(window, cx);
        let sizes = Rc::new(
            self.rows
                .iter()
                .map(|row| {
                    let lines = row
                        .node
                        .host_id()
                        .and_then(|id| host_marks.get(&id))
                        .map_or(0, |host| host.details.len());
                    size(px(0.), heights.0[lines.min(2)])
                })
                .collect::<Vec<_>>(),
        );
        let rows = self.rows.clone();
        let selected = self.selected;
        let list = v_virtual_list(
            cx.entity(),
            "host-tree-rows",
            sizes.clone(),
            move |_, range, _, cx| {
                range
                    .map(|index| {
                        let row = &rows[index];
                        let selected =
                            row_interactions.right_clicked.get().or(selected) == Some(row.node);
                        render_row(
                            row,
                            selected,
                            sizes[index].height,
                            &group_counts,
                            &host_marks,
                            &row_interactions,
                            cx,
                        )
                        .into_any_element()
                    })
                    .collect::<Vec<_>>()
            },
        )
        .track_scroll(&self.scroll_handle);

        v_flex()
            .id("host-panel")
            .key_context(HOST_PANEL_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_connect_selected))
            .on_action(
                cx.listener(|this, _: &SelectPreviousHostRow, _, cx| {
                    this.move_selection(false, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &SelectNextHostRow, _, cx| this.move_selection(true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &CollapseSelectedGroup, _, cx| this.fold_selected(false, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExpandSelectedGroup, _, cx| this.fold_selected(true, cx)),
            )
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                h_flex()
                    .p_2()
                    .gap_1()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.search)
                                .id("host-search")
                                .small()
                                .cleanable(true)
                                .prefix(Icon::new(IconName::Search).small()),
                        ),
                    )
                    .child(
                        Button::new("toggle-host-details")
                            .ghost()
                            .small()
                            .icon(Icon::new(CatalogIcon::LayoutList))
                            .selected(self.details)
                            .toggled(self.details)
                            .tooltip(t!("host.panel.show_details"))
                            .accessibility_label(t!("host.panel.show_details"))
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(ToggleHostDetails), cx)
                            }),
                    ),
            )
            .child(
                div()
                    .id("host-tree")
                    .test_support()
                    .role(Role::Tree)
                    .key_context(HOST_TREE_CONTEXT)
                    .track_focus(&self.tree_focus)
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .size_full()
                            .px_1()
                            .child(list)
                            .vertical_scrollbar(&self.scroll_handle),
                    )
                    // The menu hangs off the container, not off the rows: a
                    // row's menu would be built while the virtualized list is
                    // being prepainted, and the focus it takes there lands in
                    // the middle of the frame, which trips gpui's "set_focus
                    // called more than once in a single frame" assertion.
                    // Capture runs before every bubble handler. Clear the
                    // context selection on any new click; a right-clicked row
                    // writes itself back before the menu is built.
                    .capture_any_mouse_down(move |event, _, _| {
                        if matches!(event.button, MouseButton::Left | MouseButton::Right) {
                            clicked_blank.set(None);
                        }
                    })
                    .when(can_reorder, |view| {
                        view.on_drag_move(move |_: &DragMoveEvent<DraggedHostNode>, _, _| {
                            drop_target_for_move.set(None);
                        })
                        .on_drop(
                            move |drag: &DraggedHostNode, window, cx| {
                                if drop_target_for_root.get().is_none() {
                                    window.dispatch_action(
                                        Box::new(MoveHostNode {
                                            source: drag.node,
                                            destination: NodeDrop::Root,
                                        }),
                                        cx,
                                    );
                                }
                                drop_target_for_root.set(None);
                            },
                        )
                    })
                    .context_menu({
                        let store = self.store.clone();
                        move |menu, _, cx| {
                            build_context_menu(clicked_menu.get(), store.read(cx), menu)
                        }
                    }),
            )
            .child(self.row_tooltips.overlay())
    }
}

/// A row that is not a host still has to reserve the badge's width, or the
/// labels of groups and hosts would not line up.
fn plain_mark(icon: Icon, cx: &App) -> AnyElement {
    h_flex()
        .flex_shrink_0()
        .size_5()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .child(icon.small())
        .into_any_element()
}

struct RowInteractions {
    details: bool,
    group_parents: Rc<HashMap<GroupId, Option<GroupId>>>,
    right_clicked: Rc<Cell<Option<HostNode>>>,
    drop_target: Rc<Cell<Option<(HostNode, NodeDrop)>>>,
    can_reorder: bool,
    /// What every host's row tooltip says.
    tooltips: Rc<HashMap<HostId, RowTooltip>>,
    row_tooltips: RowTooltips<HostId>,
}

/// Count hosts in each group, including hosts in nested child groups.
fn group_host_counts(
    hosts: &[super::Host],
    parents: &HashMap<GroupId, Option<GroupId>>,
) -> HashMap<GroupId, usize> {
    let mut counts = HashMap::new();
    for host in hosts {
        let mut group = host.group;
        for _ in 0..parents.len() {
            let Some(id) = group else { break };
            *counts.entry(id).or_insert(0) += 1;
            group = parents.get(&id).copied().flatten();
        }
    }
    counts
}

/// What a host row's mark is drawn from, taken in `render`.
struct MarkOf {
    os: Option<HostOs>,
    protocol: ProtocolKind,
    address: SharedString,
    details: Vec<SharedString>,
}

fn location(host: &Host) -> SharedString {
    if !host.kind().has_address() {
        return host.endpoint().into();
    }
    let user = if host.is_ssh() && !host.user.is_empty() {
        format!("{}@", host.user)
    } else {
        String::new()
    };
    if host.address.contains(':') {
        format!("{user}[{}]:{}", host.address, host.port).into()
    } else {
        format!("{user}{}:{}", host.address, host.port).into()
    }
}

fn note_line(notes: &str) -> Option<SharedString> {
    let line = notes
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    (!line.is_empty()).then(|| line.into())
}

struct RowHeights([Pixels; 3]);

impl RowHeights {
    fn measure(window: &mut Window, cx: &mut App) -> Self {
        let mut heights = [px(0.); 3];
        for (details, height) in heights.iter_mut().enumerate() {
            let lines = (0..details)
                .map(|index| (("host-row-sample-detail", index as u64).into(), "M".into()))
                .collect();
            let content = row_content(
                plain_mark(Icon::new(IconName::Folder), cx),
                "M".into(),
                lines,
                cx,
            );
            *height = row_frame(("host-row-sample", details as u64), 0, cx)
                .child(content)
                .into_any_element()
                .layout_as_root(
                    size(AvailableSpace::MinContent, AvailableSpace::MinContent),
                    window,
                    cx,
                )
                .height;
        }
        Self(heights)
    }
}

fn row_frame(id: impl Into<ElementId>, depth: usize, cx: &App) -> ListItem {
    ListItem::new(id)
        .w_full()
        .px_2()
        .rounded(cx.theme().radius)
        .pl(rems(0.75 + depth as f32))
}

fn row_content(
    mark: AnyElement,
    label: SharedString,
    details: Vec<(ElementId, SharedString)>,
    cx: &App,
) -> Div {
    v_flex()
        .min_w_0()
        .child(
            h_flex()
                .gap_2()
                .child(mark)
                .child(div().min_w_0().truncate().child(label)),
        )
        .children(details.into_iter().map(|(id, line)| {
            h_flex().gap_2().child(div().flex_shrink_0().w_5()).child(
                div()
                    .id(id)
                    .test_support()
                    .aria_label(line.clone())
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(line),
            )
        }))
}

fn render_row(
    entry: &TreeRow,
    selected: bool,
    height: Pixels,
    group_counts: &HashMap<GroupId, usize>,
    host_marks: &HashMap<HostId, MarkOf>,
    interactions: &RowInteractions,
    cx: &mut Context<HostPanel>,
) -> ListItem {
    let RowInteractions {
        group_parents,
        right_clicked,
        drop_target,
        can_reorder,
        tooltips,
        row_tooltips,
        details,
    } = interactions;
    let item = entry;
    let node = Some(entry.node);
    let (mark, row_id): (AnyElement, ElementId) = match node {
        Some(HostNode::Group(id)) => (
            plain_mark(
                Icon::new(if entry.expanded {
                    IconName::FolderOpen
                } else {
                    IconName::Folder
                }),
                cx,
            ),
            ("group-row", id.0).into(),
        ),
        Some(HostNode::Host(id)) => (
            {
                let mark = host_marks.get(&id);
                HostMark::new(
                    ("host-os", id.0),
                    item.label.clone(),
                    mark.map(|mark| mark.address.clone()).unwrap_or_default(),
                    mark.and_then(|mark| mark.os),
                )
                .protocol(mark.map(|mark| mark.protocol).unwrap_or_default())
            }
            .small()
            .without_tooltip()
            .into_any_element(),
            ("host-row", id.0).into(),
        ),
        None => (
            plain_mark(Icon::new(IconName::File), cx),
            item.node.id().into(),
        ),
    };
    let host_id = node.and_then(HostNode::host_id);
    let lines = host_id
        .and_then(|id| host_marks.get(&id))
        .map(|host| host.details.clone())
        .unwrap_or_default();
    let detail_lines = host_id
        .map(|id| {
            ["host-address", "host-note"]
                .into_iter()
                .zip(lines)
                .map(|(name, line)| ((name, id.0).into(), line))
                .collect()
        })
        .unwrap_or_default();

    row_frame(row_id, entry.depth, cx)
        .h(height)
        .role(Role::TreeItem)
        .aria_label(item.label.clone())
        .aria_selected(selected)
        .when(entry.node.group_id().is_some(), |row| {
            row.aria_expanded(entry.expanded)
        })
        // Make selection stronger than hover against the sidebar background.
        .confirmed(selected)
        .when(selected, |row| {
            row.bg(crate::app::selected_row_color(cx.theme()))
        })
        .child(row_content(mark, item.label.clone(), detail_lines, cx))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener({
                let node = entry.node;
                move |this, _, _, cx| this.press_row(node, cx)
            }),
        )
        .when_some(node.and_then(HostNode::group_id), |row, id| {
            let count = group_counts.get(&id).copied().unwrap_or(0);
            row.suffix(move |_, cx| {
                div()
                    .id(("group-count", id.0))
                    .test_support()
                    .aria_label(tn!("host.count.hosts", count))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground.opacity(0.7))
                    .child(count.to_string())
            })
        })
        .when_some(host_id, |row, id| {
            row.on_click(move |event: &ClickEvent, window, cx| {
                if event.click_count() == 2 {
                    window.dispatch_action(Box::new(ConnectHost(id)), cx);
                }
            })
        })
        .when_some(host_id.filter(|_| !details), |row, id| {
            row_tooltips
                .row(id, tooltips.get(&id).cloned().unwrap_or_default())
                .attach(row)
        })
        .when_some(node, |row, node| {
            let right_clicked = right_clicked.clone();
            row.on_mouse_down(MouseButton::Right, move |_, _, _| {
                right_clicked.set(Some(node));
            })
        })
        .when(*can_reorder, |row| {
            row.when_some(node, |row, node| {
                let drop_target_for_move = drop_target.clone();
                let drop_target_for_style = drop_target.clone();
                let drop_target_for_drop = drop_target.clone();
                let parents_for_move = group_parents.clone();
                let label = item.label.clone();
                row.on_drag(DraggedHostNode { node, label }, |drag, _, _, cx| {
                    cx.new(|_| drag.clone())
                })
                .on_drag_move(move |event: &DragMoveEvent<DraggedHostNode>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                        let source = event.drag(cx).node;
                        let relative = (event.event.position.y - event.bounds.top())
                            / event.bounds.size.height;
                        let destination = match (source, node) {
                            (HostNode::Host(_), HostNode::Group(id)) => NodeDrop::Into(id),
                            (_, HostNode::Group(id)) if relative > 0.25 && relative < 0.75 => {
                                NodeDrop::Into(id)
                            }
                            _ if relative < 0.5 => NodeDrop::Before(node),
                            _ => NodeDrop::After(node),
                        };
                        if valid_drop(source, destination, &parents_for_move) {
                            drop_target_for_move.set(Some((node, destination)));
                        }
                    }
                })
                .drag_over::<DraggedHostNode>(move |style, drag, _, cx| {
                    if drag.node == node {
                        return style;
                    }
                    match drop_target_for_style.get() {
                        Some((target, NodeDrop::Before(_))) if target == node => {
                            style.border_t_1().border_color(cx.theme().primary)
                        }
                        Some((target, NodeDrop::After(_))) if target == node => {
                            style.border_b_1().border_color(cx.theme().primary)
                        }
                        Some((target, NodeDrop::Into(_))) if target == node => style
                            .bg(cx.theme().accent)
                            .border_1()
                            .border_color(cx.theme().primary),
                        _ => style,
                    }
                })
                .on_drop(move |drag: &DraggedHostNode, window, cx| {
                    if let Some((target, destination)) = drop_target_for_drop.get()
                        && target == node
                        && drag.node != node
                    {
                        window.dispatch_action(
                            Box::new(MoveHostNode {
                                source: drag.node,
                                destination,
                            }),
                            cx,
                        );
                    }
                    drop_target_for_drop.set(None);
                    cx.stop_propagation();
                })
            })
        })
}

#[derive(Clone)]
struct DraggedHostNode {
    node: HostNode,
    label: SharedString,
}

impl Render for DraggedHostNode {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover)
            .text_color(cx.theme().popover_foreground)
            .border_1()
            .border_color(cx.theme().border)
            .child(self.label.clone())
    }
}

/// Use the render snapshot to keep impossible group drops from looking active.
fn valid_drop(
    source: HostNode,
    destination: NodeDrop,
    parents: &HashMap<GroupId, Option<GroupId>>,
) -> bool {
    let target = match destination {
        NodeDrop::Before(target) | NodeDrop::After(target) => Some(target),
        _ => None,
    };
    if target == Some(source) {
        return false;
    }
    match source {
        HostNode::Host(_) => match destination {
            NodeDrop::Root => true,
            NodeDrop::Into(group) => parents.contains_key(&group),
            NodeDrop::Before(HostNode::Host(_)) | NodeDrop::After(HostNode::Host(_)) => true,
            _ => false,
        },
        HostNode::Group(id) => {
            let mut parent = match destination {
                NodeDrop::Before(HostNode::Group(target))
                | NodeDrop::After(HostNode::Group(target)) => match parents.get(&target) {
                    Some(parent) => *parent,
                    None => return false,
                },
                NodeDrop::Into(group) if parents.contains_key(&group) => Some(group),
                NodeDrop::Root => None,
                _ => return false,
            };
            for _ in 0..=parents.len() {
                let Some(current) = parent else {
                    return true;
                };
                if current == id {
                    return false;
                }
                parent = match parents.get(&current) {
                    Some(parent) => *parent,
                    None => return false,
                };
            }
            false
        }
    }
}

/// The menu of one host, wherever it is listed: the host tree and the
/// start page's recent hosts both build it here, so they cannot drift.
/// Built as it opens, from the host as it is then.
///
/// Only an SSH host has SFTP, and a serial host has no address to copy.
pub fn host_menu(menu: PopupMenu, host: &Host) -> PopupMenu {
    let id = host.id;
    let ssh = host.is_ssh();
    let has_address = host.kind().has_address();
    // Named after what the address is, as in the tabs' menus.
    let copy_address = if host.address_is_ip() {
        t!("host.menu.copy_ip")
    } else {
        t!("host.menu.copy_host_name")
    };
    menu.menu_with_icon(
        t!("host.menu.connect"),
        Icon::new(CatalogIcon::Plug),
        Box::new(ConnectHost(id)),
    )
    .when(ssh, |menu| {
        menu.menu_with_icon(
            t!("host.menu.open_sftp"),
            Icon::new(CatalogIcon::FolderTree),
            Box::new(OpenExplorer(id)),
        )
    })
    .separator()
    .menu_with_icon(
        t!("host.menu.edit_host"),
        Icon::new(CatalogIcon::Pencil),
        Box::new(EditHost(id)),
    )
    .menu_with_icon(
        t!("host.menu.duplicate"),
        Icon::new(CatalogIcon::CopyPlus),
        Box::new(DuplicateHost(id)),
    )
    .separator()
    .when(has_address, |menu| {
        menu.menu_with_icon(
            copy_address,
            Icon::new(CatalogIcon::ClipboardCopy),
            Box::new(CopyHostAddress(id)),
        )
    })
    .menu_with_icon(
        t!("host.menu.copy_id"),
        Icon::new(CatalogIcon::ClipboardCopy),
        Box::new(CopyHostId(id)),
    )
    .separator()
    .menu_with_icon(
        t!("common.delete"),
        Icon::new(CatalogIcon::Trash),
        Box::new(DeleteHost(id)),
    )
}

fn build_context_menu(node: Option<HostNode>, store: &HostStore, menu: PopupMenu) -> PopupMenu {
    let parent = match node {
        Some(HostNode::Host(id)) => store.host(id).and_then(|host| host.group),
        Some(HostNode::Group(id)) => Some(id),
        None => None,
    };
    let menu = match node {
        Some(HostNode::Host(id)) => match store.host(id) {
            Some(host) => host_menu(menu, host),
            None => menu,
        },
        Some(HostNode::Group(id)) => menu
            .menu_with_icon(
                t!("host.menu.connect_group"),
                Icon::new(CatalogIcon::Plug),
                Box::new(ConnectGroup(id)),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.new_host"),
                Icon::new(IconName::Plus),
                Box::new(NewHostInGroup(id)),
            )
            .menu_with_icon(
                t!("host.menu.new_subgroup"),
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewChildGroup(id)),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.expand_all"),
                Icon::new(IconName::FolderOpen),
                Box::new(ExpandAllGroups),
            )
            .menu_with_icon(
                t!("host.menu.collapse_all"),
                Icon::new(IconName::Folder),
                Box::new(CollapseAllGroups),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.rename_group"),
                Icon::new(CatalogIcon::Pencil),
                Box::new(RenameGroup(id)),
            )
            .separator()
            .menu_with_icon(
                t!("host.menu.delete_group"),
                Icon::new(CatalogIcon::Trash),
                Box::new(DeleteGroup(id)),
            ),
        None => import_menu_items(
            menu.menu_with_icon(
                t!("host.menu.new_host"),
                Icon::new(IconName::Plus),
                Box::new(NewHost),
            )
            .menu_with_icon(
                t!("host.menu.new_group"),
                Icon::new(CatalogIcon::FolderPlus),
                Box::new(NewGroup),
            )
            .separator(),
        ),
    };
    menu.when(store.hosts_manually_ordered(parent), |menu| {
        menu.separator().menu(
            t!("host.menu.automatic_order"),
            Box::new(AutomaticallyOrderHosts::new(parent)),
        )
    })
}

/// The import commands, one per source: under the blank space's menu and
/// in the dock's 「…」 menu while the host list shows.
pub fn import_menu_items(menu: PopupMenu) -> PopupMenu {
    menu.menu_with_icon(
        t!("host.menu.import_ssh_config"),
        Icon::new(CatalogIcon::Import),
        Box::new(ImportSshConfig),
    )
    .menu_with_icon(
        t!("host.menu.import_known_hosts"),
        Icon::new(CatalogIcon::Import),
        Box::new(ImportKnownHosts),
    )
}
