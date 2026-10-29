//! One window's tree: Android nodes ([`RawNode`]) mapped to AccessKit nodes,
//! and the mirror of what AccessKit holds, so every `TreeUpdate` handed to
//! it is valid.
//!
//! **Mapping** follows Chromium's ARC++ (`accessibility_node_info_data_wrapper.cc`):
//! roles from the editable flag, link spans, collection info and the Android
//! class name; the name from the content description, the labelling node,
//! the text and the hint; value, checked, expanded and range states. AccessKit
//! roles mirror Chromium's, so the tables carry over. Editable text gets one
//! text run child, which is what gives AT-SPI clients the Text interface and
//! the caret.
//!
//! **Validity.** `accesskit_consumer` panics on an update that isn't a tree
//! over what it holds (a child it doesn't know, a node nobody lists, a node
//! listed twice, a focus outside the tree), and a panic there would take the
//! adapter down. [`WindowTree::apply`] keeps a mirror of AccessKit's nodes
//! and their parents, and rewrites each snapshot into an update that keeps
//! the tree a tree: unknown children and orphans are dropped, a node moved
//! to a new parent is removed from its old one, and the focus is clamped to
//! a live node. Coordinates are logical pixels relative to the window: on
//! Wayland nothing knows where the window is.

use std::collections::{HashMap, HashSet};

use accesskit::{
    Action, ActionData, ActionRequest, CustomAction, Invalid, Live, Node, NodeId, Rect, Role,
    TextPosition, TextSelection, Toggled, TreeId, TreeInfo, TreeUpdate,
};

use super::proto::ActionEvent;
use super::wire::{RawNode, Snapshot, action as act, flag, limits};

/// The text run of an editable node is its child with this bit added.
const RUN_BIT: u64 = limits::MAX_ID;

fn run_id(id: NodeId) -> NodeId {
    NodeId(id.0 | RUN_BIT)
}

/// What the bridge keeps per node to answer actions.
#[derive(Debug, Clone, Default)]
struct Meta {
    actions: u32,
    clickable: bool,
    custom: Vec<i32>,
    /// Editable nodes: the text and selection (UTF-16 offsets).
    text: Option<(String, (i32, i32))>,
    collection: Option<CollectionKind>,
}

#[derive(Debug, Clone)]
struct Entry {
    node: Node,
    parent: Option<NodeId>,
    meta: Meta,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectionKind {
    Grid,
    ListWithCount { rows: i32, columns: i32 },
    ListWithoutCount,
}

fn collection_kind(n: &RawNode) -> Option<CollectionKind> {
    let c = n.collection?;
    if c.rows > 1 && c.columns > 1 {
        return Some(CollectionKind::Grid);
    }
    if c.rows == 1 || c.columns == 1 {
        if c.rows > 0 && c.columns > 0 {
            return Some(CollectionKind::ListWithCount {
                rows: c.rows,
                columns: c.columns,
            });
        }
        return Some(CollectionKind::ListWithoutCount);
    }
    None
}

/// The node's role: Chromium's `PopulateAXRole`, adjusted for AccessKit, which
/// leaves generic containers out of AT-SPI. A generic container that is
/// clickable (a Compose button is a clickable `View` around its text) becomes
/// a button, which AccessKit names from its contents as Chromium's
/// `ComputeNameFromContents` does; a named leaf (an icon's content
/// description) a label, so that naming sees it; and one that is focusable or
/// named a group. A button that is unnamed, inert and empty is no control
/// (Compose puts one inside each button for its role), so it is left out.
fn role(n: &RawNode, ancestor: Option<CollectionKind>) -> Role {
    let clickable = n.has(flag::CLICKABLE)
        || n.has(flag::LONG_CLICKABLE)
        || n.can(act::CLICK)
        || n.can(act::LONG_CLICK);
    let named = !name(n).is_empty();
    match chromium_role(n, ancestor) {
        Role::GenericContainer if clickable => Role::Button,
        Role::GenericContainer if named && n.children.is_empty() => Role::Label,
        Role::GenericContainer if named || n.has(flag::FOCUSABLE) => Role::Group,
        Role::Button if !clickable && !named && n.actions == 0 && n.children.is_empty() => {
            Role::GenericContainer
        }
        r => r,
    }
}

/// Chromium's `PopulateAXRole`. `ancestor` is the nearest ancestor's
/// collection, for collection items.
fn chromium_role(n: &RawNode, ancestor: Option<CollectionKind>) -> Role {
    if n.has(flag::EDITABLE) {
        return if n.has(flag::PASSWORD) {
            Role::PasswordInput
        } else if n.has(flag::MULTI_LINE) {
            Role::MultilineTextInput
        } else {
            Role::TextInput
        };
    }
    if n.has(flag::TEXT_IS_LINK) {
        return Role::Link;
    }
    match collection_kind(n) {
        Some(CollectionKind::Grid) => return Role::Grid,
        Some(_) => return Role::List,
        None => {}
    }
    if let Some(item) = n.item {
        if item.heading {
            return Role::ColumnHeader;
        }
        match ancestor {
            Some(CollectionKind::Grid) => return Role::GridCell,
            Some(_) => return Role::ListItem,
            None => {}
        }
    }
    if n.has(flag::HEADING) {
        return Role::Heading;
    }
    let by_class = match n.class_name.as_str() {
        "android.widget.AbsListView" | "android.widget.ListView" => Some(Role::List),
        "android.app.AlertDialog" => Some(Role::AlertDialog),
        "android.widget.Button" | "android.widget.ImageButton" => Some(Role::Button),
        "android.widget.CheckBox" | "android.widget.CompoundButton" => Some(Role::CheckBox),
        "android.widget.CheckedTextView" => Some(Role::Label),
        "android.widget.GridView" => Some(Role::Table),
        "android.widget.HorizontalScrollView" | "android.widget.ScrollView" => {
            Some(Role::ScrollView)
        }
        "android.widget.Image" => Some(Role::Image),
        "android.widget.ImageView" if n.has(flag::CLICKABLE) => Some(Role::Button),
        "android.widget.ImageView" => Some(Role::Image),
        "android.view.MenuItem" => Some(Role::MenuItem),
        "android.support.v4.view.ViewPager" | "androidx.viewpager.widget.ViewPager" => {
            Some(Role::Group)
        }
        "android.widget.ProgressBar" => Some(Role::ProgressIndicator),
        "android.widget.RadioButton" => Some(Role::RadioButton),
        "android.widget.RadioGroup" => Some(Role::RadioGroup),
        "android.widget.SeekBar" => Some(Role::Slider),
        "android.widget.Spinner" => Some(Role::ComboBox),
        "android.widget.Switch" => Some(Role::Switch),
        "android.widget.TabWidget" => Some(Role::TabList),
        // AccessKit has no toggle-button role: a button with a toggled
        // state is one on AT-SPI.
        "android.widget.ToggleButton" => Some(Role::Button),
        "android.view.View" => Some(Role::GenericContainer),
        "android.view.ViewGroup" => Some(Role::Group),
        _ => None,
    };
    if let Some(r) = by_class {
        return r;
    }
    if n.collection.is_some() {
        return Role::List;
    }
    if !n.text.is_empty() && n.children.is_empty() {
        Role::Label
    } else {
        Role::GenericContainer
    }
}

/// Chromium's `ComputeAXName`: content description, the labelling node's
/// name, the text unless editable, and the hint.
fn name(n: &RawNode) -> String {
    let editable = n.has(flag::EDITABLE);
    let parts = [
        n.content_description.as_str(),
        n.labeled_by.as_str(),
        if editable { "" } else { n.text.as_str() },
        n.hint.as_str(),
    ];
    let parts: Vec<&str> = parts.into_iter().filter(|s| !s.is_empty()).collect();
    parts.join(" ")
}

/// A UTF-16 offset as a char index into `s` (clamped).
fn char_index(s: &str, utf16: i32) -> usize {
    let mut units = 0usize;
    for (i, c) in s.chars().enumerate() {
        if units >= utf16.max(0) as usize {
            return i;
        }
        units += c.len_utf16();
    }
    s.chars().count()
}

/// A char index into `s` as a UTF-16 offset (clamped).
fn utf16_offset(s: &str, chars: usize) -> i32 {
    s.chars().take(chars).map(char::len_utf16).sum::<usize>() as i32
}

/// The masked form of a password's text.
fn mask(s: &str) -> String {
    "•".repeat(s.chars().count())
}

/// The text run child of an editable node: the whole value, one character
/// per Unicode scalar value.
fn text_run(value: &str, bounds: Option<Rect>) -> Node {
    let mut run = Node::new(Role::TextRun);
    run.set_value(value);
    let lengths: Vec<u8> = value.chars().map(|c| c.len_utf8() as u8).collect();
    run.set_character_lengths(lengths);
    let chars: Vec<char> = value.chars().collect();
    // Word starts are u8 character indices, so only short runs carry them.
    if chars.len() <= u8::MAX as usize {
        let starts: Vec<u8> = (0..chars.len())
            .filter(|&i| !chars[i].is_whitespace() && (i == 0 || chars[i - 1].is_whitespace()))
            .map(|i| i as u8)
            .collect();
        run.set_word_starts(starts);
    }
    if let Some(b) = bounds {
        run.set_bounds(b);
    }
    run
}

/// Builds the AccessKit node(s) for one Android node: the node, and for
/// editable text its run.
fn convert(
    n: &RawNode,
    scale: f32,
    ancestor: Option<CollectionKind>,
) -> (Node, Meta, Option<Node>) {
    let id = NodeId(n.id);
    let role = role(n, ancestor);
    let mut node = Node::new(role);
    let s = scale as f64;
    let [l, t, r, b] = n.bounds;
    let bounds = (r > l && b > t).then(|| Rect {
        x0: l as f64 / s,
        y0: t as f64 / s,
        x1: r as f64 / s,
        y1: b as f64 / s,
    });
    if let Some(b) = bounds {
        node.set_bounds(b);
    }
    let editable = n.has(flag::EDITABLE);
    let password = n.has(flag::PASSWORD);
    let name = name(n);
    if !name.is_empty() {
        // A label's name comes from its value in AccessKit.
        if role == Role::Label {
            node.set_value(name);
        } else {
            node.set_label(name);
        }
    }
    if !n.class_name.is_empty() {
        node.set_class_name(n.class_name.as_str());
    }
    if !n.view_id.is_empty() {
        node.set_author_id(n.view_id.as_str());
    }
    if !n.tooltip.is_empty() {
        node.set_tooltip(n.tooltip.as_str());
    }
    if !n.hint.is_empty() && editable {
        node.set_placeholder(n.hint.as_str());
    }
    // State description: a range's value text, a checkable's state, or else
    // a description.
    if !n.state_description.is_empty() {
        if n.range.is_some() {
            node.set_value(n.state_description.as_str());
        } else if n.has(flag::CHECKABLE) {
            node.set_state_description(n.state_description.as_str());
        } else {
            node.set_description(n.state_description.as_str());
        }
    }
    if n.has(flag::CHECKABLE) {
        node.set_toggled(if n.has(flag::CHECKED_MIXED) {
            Toggled::Mixed
        } else if n.has(flag::CHECKED) {
            Toggled::True
        } else {
            Toggled::False
        });
    }
    if !n.has(flag::ENABLED) {
        node.set_disabled();
    }
    if n.has(flag::SELECTED) {
        node.set_selected(true);
    }
    if n.has(flag::CONTENT_INVALID) {
        node.set_invalid(Invalid::True);
    }
    match n.live {
        1 => node.set_live(Live::Polite),
        2 => node.set_live(Live::Assertive),
        _ => {}
    }
    if n.can(act::EXPAND) {
        node.add_action(Action::Expand);
        node.set_expanded(false);
    }
    if n.can(act::COLLAPSE) {
        node.add_action(Action::Collapse);
        node.set_expanded(true);
    }
    if let Some(g) = n.range {
        let (min, max) = if g.kind == 3 {
            (0.0, 100.0)
        } else {
            (g.min as f64, g.max as f64)
        };
        node.set_min_numeric_value(min);
        node.set_max_numeric_value(max.max(min));
        node.set_numeric_value((g.current as f64).clamp(min, max.max(min)));
    }
    match collection_kind(n) {
        Some(CollectionKind::Grid) => {
            let c = n.collection.unwrap();
            node.set_row_count(c.rows.max(0) as usize);
            node.set_column_count(c.columns.max(0) as usize);
        }
        Some(CollectionKind::ListWithCount { rows, columns }) => {
            node.set_size_of_set(rows.max(columns) as usize);
        }
        _ => {}
    }
    if let Some(item) = n.item {
        match ancestor {
            Some(CollectionKind::Grid) if !item.heading => {
                node.set_row_index(item.row.max(0) as usize);
                node.set_column_index(item.column.max(0) as usize);
            }
            Some(CollectionKind::ListWithCount { rows, columns }) if !item.heading => {
                let index = if rows == 1 { item.column } else { item.row };
                if columns == 1 || rows == 1 {
                    node.set_position_in_set(index.max(0) as usize + 1);
                }
            }
            _ => {}
        }
    }

    // Actions. AT-SPI clients see only what's added here.
    let clickable = n.has(flag::CLICKABLE) || n.can(act::CLICK);
    if clickable {
        node.add_action(Action::Click);
    }
    if n.has(flag::FOCUSABLE) || n.can(act::FOCUS) {
        node.add_action(Action::Focus);
    }
    if n.can(act::CLEAR_FOCUS) {
        node.add_action(Action::Blur);
    }
    if n.has(flag::LONG_CLICKABLE) || n.can(act::LONG_CLICK) || n.can(act::CONTEXT_CLICK) {
        node.add_action(Action::ShowContextMenu);
    }
    let fwd = n.can(act::SCROLL_FORWARD);
    let back = n.can(act::SCROLL_BACKWARD);
    if role == Role::Slider || role == Role::SpinButton {
        if fwd {
            node.add_action(Action::Increment);
        }
        if back {
            node.add_action(Action::Decrement);
        }
    } else {
        if fwd || n.can(act::SCROLL_DOWN) {
            node.add_action(Action::ScrollDown);
        }
        if back || n.can(act::SCROLL_UP) {
            node.add_action(Action::ScrollUp);
        }
        if n.can(act::SCROLL_LEFT) {
            node.add_action(Action::ScrollLeft);
        }
        if n.can(act::SCROLL_RIGHT) {
            node.add_action(Action::ScrollRight);
        }
    }
    if n.can(act::SET_PROGRESS) || (editable && n.can(act::SET_TEXT)) {
        node.add_action(Action::SetValue);
    }
    if editable && n.can(act::SET_TEXT) {
        node.add_action(Action::ReplaceSelectedText);
    }
    if n.can(act::SHOW_ON_SCREEN) {
        node.add_action(Action::ScrollIntoView);
    }
    if n.can(act::SHOW_TOOLTIP) {
        node.add_action(Action::ShowTooltip);
    }
    if n.can(act::HIDE_TOOLTIP) {
        node.add_action(Action::HideTooltip);
    }
    if !n.custom_actions.is_empty() {
        node.add_action(Action::CustomAction);
        node.set_custom_actions(
            n.custom_actions
                .iter()
                .map(|(id, label)| CustomAction {
                    id: *id,
                    description: label.clone(),
                })
                .collect::<Vec<_>>(),
        );
    }

    let mut children: Vec<NodeId> = n.children.iter().map(|&c| NodeId(c)).collect();
    let mut run = None;
    let mut text = None;
    if editable {
        // Chromium: the value is the text unless it is the hint showing.
        let raw = if n.has(flag::SHOWING_HINT_TEXT) {
            ""
        } else {
            n.text.as_str()
        };
        let value = if password { mask(raw) } else { raw.to_owned() };
        node.set_value(value.as_str());
        if n.can(act::SET_SELECTION) {
            node.add_action(Action::SetTextSelection);
        }
        let (a, b) = n.selection;
        if a >= 0 && b >= 0 {
            let run = run_id(id);
            node.set_text_selection(TextSelection {
                anchor: TextPosition {
                    node: run,
                    character_index: char_index(raw, a),
                },
                focus: TextPosition {
                    node: run,
                    character_index: char_index(raw, b),
                },
            });
        }
        children.push(run_id(id));
        run = Some(text_run(&value, bounds));
        text = Some((raw.to_owned(), n.selection));
    }
    node.set_children(children);
    let meta = Meta {
        actions: n.actions,
        clickable,
        custom: n.custom_actions.iter().map(|(id, _)| *id).collect(),
        text,
        collection: collection_kind(n),
    };
    (node, meta, run)
}

/// One window's tree, as AccessKit holds it.
#[derive(Default)]
pub struct WindowTree {
    nodes: HashMap<NodeId, Entry>,
    root: Option<NodeId>,
    focus: Option<NodeId>,
}

impl WindowTree {
    #[cfg(test)]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    #[cfg(test)]
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// The whole tree as one update: what AccessKit starts from.
    pub fn full_update(&self) -> Option<TreeUpdate> {
        let root = self.root?;
        Some(TreeUpdate {
            nodes: self
                .nodes
                .iter()
                .map(|(id, e)| (*id, e.node.clone()))
                .collect(),
            tree: Some(tree_info(root)),
            tree_id: TreeId::ROOT,
            focus: self.focus.unwrap_or(root),
        })
    }

    /// The nearest collection above `id`, looking at the snapshot's new
    /// parents first.
    fn ancestor_collection(
        &self,
        id: NodeId,
        parents: &HashMap<NodeId, NodeId>,
        incoming: &HashMap<NodeId, &RawNode>,
    ) -> Option<CollectionKind> {
        let mut at = id;
        for _ in 0..256 {
            at = match parents.get(&at) {
                Some(p) => *p,
                None => self.nodes.get(&at)?.parent?,
            };
            let kind = match incoming.get(&at) {
                Some(n) => collection_kind(n),
                None => self.nodes.get(&at)?.meta.collection,
            };
            if kind.is_some() {
                return kind;
            }
        }
        None
    }

    /// Applies a snapshot; returns the update to hand AccessKit. A partial
    /// snapshot needs a full one first.
    pub fn apply(&mut self, s: &Snapshot) -> Result<TreeUpdate, &'static str> {
        let full = !s.partial;
        if !full && self.root.is_none() {
            return Err("partial snapshot before a full one");
        }
        let mut seen = HashSet::new();
        if !s.nodes.iter().all(|n| seen.insert(n.id)) {
            return Err("node sent twice");
        }
        let root = if full {
            NodeId(s.nodes[0].id)
        } else {
            self.root.unwrap()
        };
        // In a full snapshot nothing from before counts: it must stand alone.
        let empty = HashMap::new();
        let before = if full { &empty } else { &self.nodes };

        // 1. Who claims which child: the first claim wins; the root, unknown
        //    ids and self-references are dropped.
        let incoming: HashMap<NodeId, &RawNode> =
            s.nodes.iter().map(|n| (NodeId(n.id), n)).collect();
        let mut kept: Vec<&RawNode> = s.nodes.iter().collect();
        let mut claims: HashMap<NodeId, Vec<NodeId>>;
        let mut banned: HashSet<(NodeId, NodeId)> = HashSet::new();
        loop {
            let present: HashSet<NodeId> = kept.iter().map(|n| NodeId(n.id)).collect();
            claims = HashMap::new();
            let mut parent_of: HashMap<NodeId, NodeId> = HashMap::new();
            for n in &kept {
                let p = NodeId(n.id);
                let mut list = Vec::new();
                for &c in &n.children {
                    let c = NodeId(c);
                    if c == root || c == p || parent_of.contains_key(&c) || banned.contains(&(p, c))
                    {
                        continue;
                    }
                    if !(present.contains(&c) || before.contains_key(&c)) {
                        continue;
                    }
                    parent_of.insert(c, p);
                    list.push(c);
                }
                claims.insert(p, list);
            }
            // A claim that makes a node its own ancestor is dropped.
            let parent = |id: NodeId| -> Option<NodeId> {
                parent_of
                    .get(&id)
                    .copied()
                    .or_else(|| before.get(&id).and_then(|e| e.parent))
            };
            let mut cyclic = Vec::new();
            for (&c, &p) in &parent_of {
                let mut at = Some(p);
                let mut seen = HashSet::new();
                while let Some(x) = at {
                    if x == c {
                        cyclic.push((p, c));
                        break;
                    }
                    if !seen.insert(x) {
                        break;
                    }
                    at = parent(x);
                }
            }
            for (p, c) in &cyclic {
                claims.get_mut(p).unwrap().retain(|x| x != c);
                parent_of.remove(c);
                banned.insert((*p, *c));
            }
            // 2. A node must stay attached: the root, claimed now, or still
            //    listed by a parent that isn't re-sent.
            let attached = |n: &&RawNode| {
                let id = NodeId(n.id);
                id == root
                    || parent_of.contains_key(&id)
                    || before
                        .get(&id)
                        .and_then(|e| e.parent)
                        .is_some_and(|p| !present.contains(&p))
            };
            let n0 = kept.len();
            kept.retain(attached);
            if kept.len() == n0 && cyclic.is_empty() {
                break;
            }
        }

        // 3. The nodes, with their new parents; a moved node's old parent
        //    (not re-sent) is re-sent without it.
        let mut parents: HashMap<NodeId, NodeId> = HashMap::new();
        for (p, cs) in &claims {
            for c in cs {
                parents.insert(*c, *p);
            }
        }
        let mut out: Vec<(NodeId, Node)> = Vec::new();
        let mut entries: Vec<(NodeId, Entry)> = Vec::new();
        for n in &kept {
            let id = NodeId(n.id);
            let ancestor = self.ancestor_collection(id, &parents, &incoming);
            let (mut node, meta, run) = convert(n, s.scale, ancestor);
            let mut children = claims.remove(&id).unwrap_or_default();
            if let Some(run) = run {
                children.push(run_id(id));
                out.push((run_id(id), run.clone()));
                entries.push((
                    run_id(id),
                    Entry {
                        node: run,
                        parent: Some(id),
                        meta: Meta::default(),
                    },
                ));
            }
            node.set_children(children);
            if id == root {
                node.set_role(Role::Window);
                if !s.title.is_empty() {
                    node.set_label(s.title.as_str());
                }
            }
            out.push((id, node.clone()));
            entries.push((
                id,
                Entry {
                    node,
                    parent: if id == root {
                        None
                    } else {
                        parents
                            .get(&id)
                            .copied()
                            .or_else(|| before.get(&id)?.parent)
                    },
                    meta,
                },
            ));
        }
        let present: HashSet<NodeId> = kept.iter().map(|n| NodeId(n.id)).collect();
        let mut amended: HashMap<NodeId, Node> = HashMap::new();
        for (&c, &p) in &parents {
            if let Some(old) = before.get(&c).and_then(|e| e.parent)
                && old != p
                && !present.contains(&old)
            {
                let node = amended
                    .entry(old)
                    .or_insert_with(|| before[&old].node.clone());
                let rest: Vec<NodeId> = node
                    .children()
                    .iter()
                    .copied()
                    .filter(|x| *x != c)
                    .collect();
                node.set_children(rest);
            }
        }
        for (id, node) in amended {
            out.push((id, node.clone()));
            let mut e = before[&id].clone();
            e.node = node;
            entries.push((id, e));
        }

        // 4. The mirror: what is reachable from the root now.
        let mut next: HashMap<NodeId, Entry> = if full {
            HashMap::new()
        } else {
            std::mem::take(&mut self.nodes)
        };
        for (id, e) in entries {
            next.insert(id, e);
        }
        let mut live = HashMap::with_capacity(next.len());
        let mut stack = vec![(root, None)];
        while let Some((id, parent)) = stack.pop() {
            let Some(mut e) = next.remove(&id) else {
                continue;
            };
            e.parent = parent;
            stack.extend(e.node.children().iter().map(|c| (*c, Some(id))));
            live.insert(id, e);
        }
        self.nodes = live;
        self.root = Some(root);

        let wanted = (s.focus != 0).then_some(NodeId(s.focus));
        let focus = match wanted {
            Some(f) if self.nodes.contains_key(&f) => f,
            _ if !full => self
                .focus
                .filter(|f| self.nodes.contains_key(f))
                .unwrap_or(root),
            _ => root,
        };
        self.focus = Some(focus);
        Ok(TreeUpdate {
            nodes: out
                .into_iter()
                .filter(|(id, _)| self.nodes.contains_key(id))
                .collect(),
            tree: full.then(|| tree_info(root)),
            tree_id: TreeId::ROOT,
            focus,
        })
    }

    /// What an AT-SPI action request asks of Android, if the node can do it.
    pub fn action(&self, window: u32, r: &ActionRequest) -> Option<ActionEvent> {
        let e = self.nodes.get(&r.target_node)?;
        let m = &e.meta;
        let can = |a: u8| m.actions & act::bit(a) != 0;
        let mut ev = ActionEvent {
            window,
            node: r.target_node.0,
            action: 0,
            a: 0,
            b: 0,
            value: 0.0,
            text: String::new(),
        };
        let pick = |want: u8, fallback: u8| if can(want) { want } else { fallback };
        ev.action = match (&r.action, &r.data) {
            (Action::Click, _) if m.clickable => act::CLICK,
            (Action::Focus, _) => act::FOCUS,
            (Action::Blur, _) => act::CLEAR_FOCUS,
            (Action::ShowContextMenu, _) => pick(act::LONG_CLICK, act::CONTEXT_CLICK),
            (Action::Expand, _) => act::EXPAND,
            (Action::Collapse, _) => act::COLLAPSE,
            (Action::Increment, _) => act::SCROLL_FORWARD,
            (Action::Decrement, _) => act::SCROLL_BACKWARD,
            (Action::ScrollDown, _) => pick(act::SCROLL_DOWN, act::SCROLL_FORWARD),
            (Action::ScrollUp, _) => pick(act::SCROLL_UP, act::SCROLL_BACKWARD),
            (Action::ScrollRight, _) => pick(act::SCROLL_RIGHT, act::SCROLL_FORWARD),
            (Action::ScrollLeft, _) => pick(act::SCROLL_LEFT, act::SCROLL_BACKWARD),
            (Action::ScrollIntoView, _) => act::SHOW_ON_SCREEN,
            (Action::ShowTooltip, _) => act::SHOW_TOOLTIP,
            (Action::HideTooltip, _) => act::HIDE_TOOLTIP,
            (Action::SetValue, Some(ActionData::NumericValue(v))) => {
                ev.value = *v as f32;
                act::SET_PROGRESS
            }
            (Action::SetValue, Some(ActionData::Value(t))) => {
                ev.text = t.to_string();
                act::SET_TEXT
            }
            (Action::ReplaceSelectedText, Some(ActionData::Value(t))) => {
                let (text, (a, b)) = m.text.as_ref()?;
                let (a, b) = (*a.min(b), *a.max(b));
                let (a, b) = (char_index(text, a), char_index(text, b));
                let head: String = text.chars().take(a).collect();
                let tail: String = text.chars().skip(b).collect();
                ev.text = format!("{head}{t}{tail}");
                act::SET_TEXT
            }
            (Action::SetTextSelection, Some(ActionData::SetTextSelection(sel))) => {
                let (text, _) = m.text.as_ref()?;
                let run = run_id(r.target_node);
                if sel.anchor.node != run || sel.focus.node != run {
                    return None;
                }
                ev.a = utf16_offset(text, sel.anchor.character_index);
                ev.b = utf16_offset(text, sel.focus.character_index);
                act::SET_SELECTION
            }
            (Action::CustomAction, Some(ActionData::CustomAction(id))) => {
                if !m.custom.contains(id) {
                    return None;
                }
                ev.a = *id;
                act::CUSTOM
            }
            _ => return None,
        };
        // Android refuses what the node doesn't list, except clicks on
        // clickable views and focus, which views take without listing them.
        let listed = matches!(ev.action, act::CLICK | act::FOCUS | act::CUSTOM) || can(ev.action);
        listed.then_some(ev)
    }
}

fn tree_info(root: NodeId) -> TreeInfo {
    let mut t = TreeInfo::new(root);
    t.toolkit_name = Some("Android".into());
    t
}

#[cfg(test)]
mod tests {
    use super::super::wire::{Collection, Item, Range};
    use super::*;

    fn raw(id: u64) -> RawNode {
        RawNode {
            id,
            flags: flag::ENABLED | flag::VISIBLE_TO_USER,
            bounds: [0, 0, 10, 10],
            selection: (-1, -1),
            ..Default::default()
        }
    }

    fn one(n: &RawNode) -> Node {
        convert(n, 1.0, None).0
    }

    // ---- ported from Chromium's accessibility_node_info_data_wrapper_unittest.cc

    #[test]
    fn name() {
        let mut n = raw(1);
        assert_eq!(one(&n).label(), None);
        n.text = "label text".into();
        assert_eq!(one(&n).value(), Some("label text")); // a label: name in value
        n.children = vec![2];
        assert_eq!(one(&n).label(), Some("label text"));
        n.text.clear();
        n.content_description = "label content description".into();
        assert_eq!(one(&n).label(), Some("label content description"));
        n.text = "label text".into();
        assert_eq!(
            one(&n).label(),
            Some("label content description label text")
        );
    }

    #[test]
    fn text_field_name_and_value() {
        // (content description, text, hint, showing hint) -> (name, value)
        let cases = [
            (
                ("email", "editing_text", "", false),
                ("email", "editing_text"),
            ),
            (("email", "", "", false), ("email", "")),
            (("", "editing_text", "", false), ("", "editing_text")),
            (
                ("", "editing_text", "hint@example.com", false),
                ("hint@example.com", "editing_text"),
            ),
            (
                ("", "hint@example.com", "hint@example.com", true),
                ("hint@example.com", ""),
            ),
            (
                ("", "example@example.com", "example@example.com", false),
                ("example@example.com", "example@example.com"),
            ),
            (
                ("email", "hint@example.com", "hint@example.com", true),
                ("email hint@example.com", ""),
            ),
            (
                ("email", "editing_text", "hint@example.com", false),
                ("email hint@example.com", "editing_text"),
            ),
            (("", "", "", false), ("", "")),
        ];
        for ((desc, text, hint, showing), (name, value)) in cases {
            let mut n = raw(1);
            n.flags |= flag::EDITABLE;
            if showing {
                n.flags |= flag::SHOWING_HINT_TEXT;
            }
            n.content_description = desc.into();
            n.text = text.into();
            n.hint = hint.into();
            let (node, _, run) = convert(&n, 1.0, None);
            assert_eq!(
                node.label().unwrap_or(""),
                name,
                "{desc:?} {text:?} {hint:?}"
            );
            assert_eq!(node.value().unwrap_or(""), value);
            assert_eq!(run.unwrap().value().unwrap_or(""), value);
        }
    }

    #[test]
    fn states() {
        let mut n = raw(1);
        n.flags |= flag::CHECKABLE;
        assert_eq!(one(&n).toggled(), Some(Toggled::False));
        n.flags |= flag::CHECKED;
        assert_eq!(one(&n).toggled(), Some(Toggled::True));
        n.actions = act::bit(act::EXPAND);
        assert_eq!(one(&n).is_expanded(), Some(false));
        n.actions = act::bit(act::COLLAPSE);
        assert_eq!(one(&n).is_expanded(), Some(true));
        n.flags &= !flag::ENABLED;
        assert!(one(&n).is_disabled());
    }

    #[test]
    fn grid_and_list_roles() {
        let mut grid = raw(1);
        grid.collection = Some(Collection {
            rows: 2,
            columns: 2,
            hierarchical: false,
            selection_mode: 0,
        });
        let g = one(&grid);
        assert_eq!(g.role(), Role::Grid);
        assert_eq!((g.row_count(), g.column_count()), (Some(2), Some(2)));
        let mut cell = raw(2);
        cell.item = Some(Item {
            row: 1,
            column: 0,
            row_span: 1,
            column_span: 1,
            heading: false,
        });
        let c = convert(&cell, 1.0, collection_kind(&grid)).0;
        assert_eq!(c.role(), Role::GridCell);
        assert_eq!((c.row_index(), c.column_index()), (Some(1), Some(0)));

        // Vertical, horizontal, one item, no count.
        for (rows, columns, size) in [
            (4, 1, Some(4)),
            (1, 3, Some(3)),
            (1, 1, Some(1)),
            (0, 1, None),
        ] {
            let mut list = raw(1);
            list.collection = Some(Collection {
                rows,
                columns,
                hierarchical: false,
                selection_mode: 0,
            });
            let l = one(&list);
            assert_eq!(l.role(), Role::List, "{rows}x{columns}");
            assert_eq!(l.size_of_set(), size);
            let mut item = raw(2);
            item.item = Some(Item {
                row: if columns == 1 { 2 } else { 0 },
                column: if rows == 1 { 2 } else { 0 },
                row_span: 1,
                column_span: 1,
                heading: false,
            });
            let i = convert(&item, 1.0, collection_kind(&list)).0;
            assert_eq!(i.role(), Role::ListItem);
            assert_eq!(i.position_in_set(), size.map(|_| 3));
        }
        let mut heading = raw(2);
        heading.item = Some(Item {
            row: 0,
            column: 0,
            row_span: 1,
            column_span: 1,
            heading: true,
        });
        assert_eq!(one(&heading).role(), Role::ColumnHeader);
    }

    #[test]
    fn edit_text_role() {
        let mut n = raw(1);
        n.class_name = "android.widget.EditText".into();
        n.flags |= flag::EDITABLE;
        assert_eq!(chromium_role(&n, None), Role::TextInput);
        n.flags &= !flag::EDITABLE;
        n.text = "text".into();
        assert_eq!(chromium_role(&n, None), Role::Label);
        n.children = vec![2];
        assert_eq!(chromium_role(&n, None), Role::GenericContainer);
        n.flags |= flag::EDITABLE | flag::MULTI_LINE;
        assert_eq!(chromium_role(&n, None), Role::MultilineTextInput);
        n.flags |= flag::PASSWORD;
        assert_eq!(chromium_role(&n, None), Role::PasswordInput);
    }

    #[test]
    fn checked_text_view_role() {
        let mut n = raw(1);
        n.class_name = "android.widget.CheckedTextView".into();
        assert_eq!(one(&n).role(), Role::Label);
        n.children = vec![2];
        assert_eq!(one(&n).role(), Role::Label);
    }

    #[test]
    fn class_roles() {
        for (class, clickable, want) in [
            ("android.widget.Button", false, Role::Button),
            ("android.widget.CheckBox", false, Role::CheckBox),
            ("android.widget.ImageView", false, Role::Image),
            ("android.widget.ImageView", true, Role::Button),
            ("android.widget.SeekBar", false, Role::Slider),
            ("android.widget.Spinner", false, Role::ComboBox),
            ("android.widget.Switch", false, Role::Switch),
            ("android.view.ViewGroup", false, Role::Group),
            ("android.view.View", false, Role::GenericContainer),
            ("android.widget.FrameLayout", false, Role::GenericContainer),
        ] {
            let mut n = raw(1);
            n.class_name = class.into();
            if clickable {
                n.flags |= flag::CLICKABLE;
            }
            assert_eq!(chromium_role(&n, None), want, "{class}");
        }
        let mut n = raw(1);
        n.flags |= flag::HEADING;
        n.text = "Title".into();
        assert_eq!(one(&n).role(), Role::Heading);
        n.flags = flag::TEXT_IS_LINK;
        assert_eq!(one(&n).role(), Role::Link);
    }

    #[test]
    fn clickable_containers_stay_on_atspi() {
        let mut n = raw(1);
        n.class_name = "android.view.View".into();
        n.children = vec![2];
        assert_eq!(one(&n).role(), Role::GenericContainer);
        n.flags |= flag::FOCUSABLE;
        assert_eq!(one(&n).role(), Role::Group);
        n.flags |= flag::CLICKABLE;
        assert_eq!(one(&n).role(), Role::Button);
        let mut icon = raw(4);
        icon.class_name = "android.view.View".into();
        icon.content_description = "Forward".into();
        assert_eq!(one(&icon).role(), Role::Label);
        icon.children = vec![5];
        assert_eq!(one(&icon).role(), Role::Group);
        let mut marker = raw(4);
        marker.class_name = "android.widget.Button".into();
        assert_eq!(one(&marker).role(), Role::GenericContainer);
        // A button named by its text child, as AT-SPI clients see it.
        n.id = 2;
        let mut t = WindowTree::default();
        let mut text = raw(3);
        text.text = "Get started".into();
        let u = t
            .apply(&full(
                vec![with_children(raw(1), &[2]), with_children(n, &[3]), text],
                0,
            ))
            .unwrap();
        let c = accesskit_consumer::Tree::new(u, true);
        let b = c
            .state()
            .node_by_id(c.state().root().child_ids().next().unwrap())
            .unwrap();
        assert_eq!(b.label().as_deref(), Some("Get started"));
    }

    #[test]
    fn state_description() {
        let mut n = raw(1);
        n.state_description = "state description".into();
        assert_eq!(one(&n).description(), Some("state description"));
        n.range = Some(Range {
            kind: 1,
            min: 0.0,
            max: 10.0,
            current: 5.0,
        });
        let r = one(&n);
        assert_eq!(
            (r.description(), r.value()),
            (None, Some("state description"))
        );
        assert_eq!(r.numeric_value(), Some(5.0));
        n.range = None;
        n.flags |= flag::CHECKABLE;
        let c = one(&n);
        assert_eq!(c.description(), None);
        assert_eq!(c.state_description(), Some("state description"));
    }

    #[test]
    fn passwords_are_masked() {
        let mut n = raw(1);
        n.flags |= flag::EDITABLE | flag::PASSWORD;
        n.text = "hunter2".into();
        let (node, _, run) = convert(&n, 1.0, None);
        assert_eq!(node.value(), Some("•••••••"));
        assert_eq!(run.unwrap().value(), Some("•••••••"));
    }

    #[test]
    fn bounds_are_logical() {
        let mut n = raw(1);
        n.bounds = [30, 60, 90, 120];
        let b = convert(&n, 1.5, None).0.bounds().unwrap();
        assert_eq!((b.x0, b.y0, b.x1, b.y1), (20.0, 40.0, 60.0, 80.0));
    }

    // ---- the tree

    fn full(nodes: Vec<RawNode>, focus: u64) -> Snapshot {
        Snapshot {
            partial: false,
            scale: 1.0,
            focus,
            title: "App".into(),
            nodes,
        }
    }

    fn with_children(mut n: RawNode, c: &[u64]) -> RawNode {
        n.children = c.to_vec();
        n
    }

    struct Nop;
    impl accesskit_consumer::TreeChangeHandler for Nop {
        fn node_added(&mut self, _: &accesskit_consumer::NodeRef) {}
        fn node_updated(
            &mut self,
            _: &accesskit_consumer::NodeRef,
            _: &accesskit_consumer::NodeRef,
        ) {
        }
        fn focus_moved(
            &mut self,
            _: Option<&accesskit_consumer::NodeRef>,
            _: Option<&accesskit_consumer::NodeRef>,
        ) {
        }
        fn node_removed(&mut self, _: &accesskit_consumer::NodeRef) {}
    }

    fn reachable(tree: &accesskit_consumer::Tree) -> HashSet<NodeId> {
        let st = tree.state();
        let mut out = HashSet::new();
        let mut stack = vec![st.root().id()];
        while let Some(id) = stack.pop() {
            let n = st.node_by_id(id).unwrap();
            out.insert(n.locate().0);
            stack.extend(n.child_ids());
        }
        out
    }

    #[test]
    fn root_is_the_window_and_runs_are_children() {
        let mut t = WindowTree::default();
        let mut edit = raw(3);
        edit.flags |= flag::EDITABLE | flag::FOCUSED;
        edit.text = "hi".into();
        edit.selection = (2, 2);
        let u = t
            .apply(&full(
                vec![
                    with_children(raw(1), &[2]),
                    with_children(raw(2), &[3]),
                    edit,
                ],
                3,
            ))
            .unwrap();
        assert_eq!(u.focus, NodeId(3));
        let tree = accesskit_consumer::Tree::new(u, true);
        let root = tree.state().root();
        assert_eq!(root.role(), Role::Window);
        assert_eq!(root.label().as_deref(), Some("App"));
        assert!(tree.state().has_node(tree.state().focus_id_in_tree()));
        assert_eq!(t.node_count(), 4);
        assert!(t.contains(run_id(NodeId(3))));
    }

    #[test]
    fn a_moved_node_leaves_its_old_parent() {
        let mut t = WindowTree::default();
        let u0 = t
            .apply(&full(
                vec![
                    with_children(raw(1), &[2, 3]),
                    with_children(raw(2), &[4]),
                    raw(3),
                    raw(4),
                ],
                0,
            ))
            .unwrap();
        let mut c = accesskit_consumer::Tree::new(u0, true);
        // 4 moves under 3; 2 isn't re-sent.
        let mut s = full(vec![with_children(raw(3), &[4])], 4);
        s.partial = true;
        let u = t.apply(&s).unwrap();
        assert!(
            u.nodes
                .iter()
                .any(|(id, n)| *id == NodeId(2) && n.children().is_empty())
        );
        c.update_and_process_changes(u, &mut Nop);
        // Now 3 drops 4: it must go away without a panic.
        let mut s = full(vec![raw(3)], 0);
        s.partial = true;
        c.update_and_process_changes(t.apply(&s).unwrap(), &mut Nop);
        assert!(!t.contains(NodeId(4)));
        assert_eq!(reachable(&c).len(), t.node_count());
    }

    #[test]
    fn bad_references_are_dropped() {
        let mut t = WindowTree::default();
        // 9 doesn't exist, 3 is listed twice, 5 is an orphan, 1 is the root.
        let u = t
            .apply(&full(
                vec![
                    with_children(raw(1), &[2, 9, 3]),
                    with_children(raw(2), &[3, 1, 2]),
                    raw(3),
                    raw(5),
                ],
                5,
            ))
            .unwrap();
        assert_eq!(
            u.focus,
            NodeId(1),
            "focus on a dropped node goes to the root"
        );
        let c = accesskit_consumer::Tree::new(u, true);
        assert_eq!(reachable(&c).len(), 3);
        let mut s = full(vec![raw(7)], 0);
        s.partial = true;
        let u = t.apply(&s).unwrap();
        assert!(
            u.nodes.is_empty(),
            "an orphan in a partial snapshot is dropped"
        );
        // A cycle: 2 claims its parent's parent.
        let mut s = full(
            vec![with_children(raw(3), &[1]), with_children(raw(2), &[3])],
            0,
        );
        s.partial = true;
        let mut c = c;
        c.update_and_process_changes(t.apply(&s).unwrap(), &mut Nop);
        assert_eq!(reachable(&c).len(), t.node_count());
        assert!(WindowTree::default().apply(&s).is_err());
    }

    #[test]
    fn actions() {
        let mut t = WindowTree::default();
        let mut button = raw(2);
        button.flags |= flag::CLICKABLE;
        button.custom_actions = vec![(5, "Archive".into())];
        let mut edit = raw(3);
        edit.flags |= flag::EDITABLE;
        edit.actions = act::bit(act::SET_TEXT) | act::bit(act::SET_SELECTION);
        edit.text = "a😀bc".into();
        edit.selection = (3, 4);
        t.apply(&full(vec![with_children(raw(1), &[2, 3]), button, edit], 0))
            .unwrap();
        let req = |node: u64, action: Action, data: Option<ActionData>| ActionRequest {
            action,
            target_tree: TreeId::ROOT,
            target_node: NodeId(node),
            data,
        };
        assert_eq!(
            t.action(7, &req(2, Action::Click, None)).unwrap().action,
            act::CLICK
        );
        assert!(t.action(7, &req(1, Action::Click, None)).is_none());
        assert!(t.action(7, &req(2, Action::Expand, None)).is_none());
        let c = t
            .action(
                7,
                &req(2, Action::CustomAction, Some(ActionData::CustomAction(5))),
            )
            .unwrap();
        assert_eq!((c.action, c.a, c.window), (act::CUSTOM, 5, 7));
        assert!(
            t.action(
                7,
                &req(2, Action::CustomAction, Some(ActionData::CustomAction(6)))
            )
            .is_none()
        );
        // The selection covers "b"; UTF-16 offsets on the wire.
        let r = t
            .action(
                7,
                &req(
                    3,
                    Action::ReplaceSelectedText,
                    Some(ActionData::Value("XY".into())),
                ),
            )
            .unwrap();
        assert_eq!((r.action, r.text.as_str()), (act::SET_TEXT, "a😀XYc"));
        let run = run_id(NodeId(3));
        let sel = TextSelection {
            anchor: TextPosition {
                node: run,
                character_index: 2,
            },
            focus: TextPosition {
                node: run,
                character_index: 4,
            },
        };
        let s = t
            .action(
                7,
                &req(
                    3,
                    Action::SetTextSelection,
                    Some(ActionData::SetTextSelection(sel)),
                ),
            )
            .unwrap();
        assert_eq!((s.action, s.a, s.b), (act::SET_SELECTION, 3, 5));
    }
}
