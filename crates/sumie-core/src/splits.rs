//! Split topology, focus, and device-pixel geometry for one tab.

#[cfg(test)]
mod tests;

/// A half-open rectangle in device pixels, relative to the window origin.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub const fn contains(self, [x, y]: [u32; 2]) -> bool {
        x >= self.x && y >= self.y && x - self.x < self.width && y - self.y < self.height
    }

    const fn span(self, axis: Axis) -> Span {
        match axis {
            Axis::Horizontal => Span {
                start: self.x,
                len: self.width,
            },
            Axis::Vertical => Span {
                start: self.y,
                len: self.height,
            },
        }
    }

    const fn with_span(mut self, axis: Axis, start: u32, len: u32) -> Self {
        match axis {
            Axis::Horizontal => {
                self.x = start;
                self.width = len;
            }
            Axis::Vertical => {
                self.y = start;
                self.height = len;
            }
        }
        self
    }

    fn clipped(self) -> Self {
        Self {
            width: self.width.min(u32::MAX - self.x),
            height: self.height.min(u32::MAX - self.y),
            ..self
        }
    }

    const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

#[derive(Clone, Copy)]
struct Span {
    start: u32,
    len: u32,
}

impl Span {
    const fn end(self) -> u32 {
        self.start + self.len
    }

    fn overlap(self, other: Self) -> u32 {
        self.end()
            .min(other.end())
            .saturating_sub(self.start.max(other.start))
    }

    fn forward_gap(self, other: Self, forward: bool) -> Option<u32> {
        if forward {
            other.start.checked_sub(self.end())
        } else {
            self.start.checked_sub(other.end())
        }
    }

    fn doubled_center(self) -> u64 {
        2 * u64::from(self.start) + u64::from(self.len)
    }
}

/// The coordinate axis partitioned by a split (perpendicular to its divider).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// Put the new pane to the right.
    Horizontal,
    /// Put the new pane below.
    Vertical,
}

impl Axis {
    const fn cross(self) -> Self {
        match self {
            Self::Horizontal => Self::Vertical,
            Self::Vertical => Self::Horizontal,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    const fn axis(self) -> Axis {
        match self {
            Self::Left | Self::Right => Axis::Horizontal,
            Self::Up | Self::Down => Axis::Vertical,
        }
    }

    const fn forward(self) -> bool {
        matches!(self, Self::Right | Self::Down)
    }
}

/// A visible one-pixel divider. The ID is stable within its tree until removed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Divider {
    pub id: u64,
    pub rect: Rect,
    pub axis: Axis,
    start: u32,
    available: u32,
}

impl Divider {
    const fn position(self) -> u32 {
        self.rect.span(self.axis).start
    }
}

/// Reusable layout buffers. Panes are in tree order, including empty rectangles
/// when the window is too small. Zoom emits only the focused pane.
#[derive(Default)]
pub struct Layout {
    pub panes: Vec<(u64, Rect)>,
    /// Unzoomed sizes of the panes hidden behind the zoomed pane.
    pub hidden: Vec<(u64, Rect)>,
    pub dividers: Vec<Divider>,
}

impl Layout {
    fn pane(&self, id: u64) -> Option<Rect> {
        self.panes
            .iter()
            .find_map(|&(pane, rect)| (pane == id && !rect.is_empty()).then_some(rect))
    }
}

/// A nonempty binary tree with caller-assigned, unique pane IDs.
pub struct SplitTree {
    root: Node,
    focused: u64,
    zoomed: bool,
    next_divider: u64,
}

impl SplitTree {
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self {
            root: Node::Leaf(id),
            focused: id,
            zoomed: false,
            next_divider: 0,
        }
    }

    #[must_use]
    pub const fn focused(&self) -> u64 {
        self.focused
    }

    /// Return the new pane's bounds after a split, without changing the tree.
    /// Like [`Self::split`], this uses the unzoomed geometry.
    #[must_use]
    pub fn split_preview(&self, axis: Axis, bounds: Rect) -> Rect {
        let pane = self.root.focused_rect(self.focused, bounds.clipped());
        partition(pane, axis, Ratio::EVEN)[2]
    }

    /// Split the focused pane evenly, focus `new_id`, and exit zoom.
    /// The caller must supply an ID that is not already in the tree.
    pub fn split(&mut self, axis: Axis, new_id: u64) {
        let leaf = self.root.focused_leaf_mut(self.focused);
        *leaf = Node::Split(Box::new(Branch {
            id: self.next_divider,
            axis,
            ratio: Ratio::EVEN,
            first: Node::Leaf(self.focused),
            second: Node::Leaf(new_id),
        }));
        self.next_divider += 1;
        self.focused = new_id;
        self.zoomed = false;
    }

    /// Remove a leaf and promote its sibling. If closing the focused leaf,
    /// focus the promoted subtree's boundary leaf and exit zoom.
    /// An unknown ID or the last remaining leaf is left alone.
    pub fn close(&mut self, id: u64) {
        if let Some(replacement) = self.root.remove(id)
            && (self.focused == id || matches!(self.root, Node::Leaf(_)))
        {
            self.focused = replacement;
            self.zoomed = false;
        }
    }

    /// Focus an existing pane, exiting zoom if focus changes.
    /// Return whether focus changed.
    pub fn focus(&mut self, id: u64) -> bool {
        if self.focused == id || self.root.leaf_mut(id).is_none() {
            return false;
        }
        self.focused = id;
        self.zoomed = false;
        true
    }

    /// Toggle whether only the focused pane is laid out. A lone pane is unchanged.
    pub fn toggle_zoom(&mut self) {
        if matches!(self.root, Node::Split(_)) {
            self.zoomed = !self.zoomed;
        }
    }

    /// Focus an adjacent visible pane in `direction`. Prefer the largest shared
    /// edge, then the closest center on that edge, then layout order.
    /// Use a layout from this tree's current geometry.
    pub fn focus_neighbor(&mut self, direction: Direction, layout: &Layout) -> bool {
        if self.zoomed {
            return false;
        }
        let Some(pane) = layout.pane(self.focused) else {
            return false;
        };
        let axis = direction.axis();
        let along = pane.span(axis);
        let across = pane.span(axis.cross());
        let forward = direction.forward();
        let neighbor = layout
            .panes
            .iter()
            .filter_map(|&(id, rect)| {
                neighbor_score(rect, axis, along, across, forward).map(|score| (score, id))
            })
            .min_by_key(|&(score, _)| score);
        neighbor.is_some_and(|(_, id)| self.focus(id))
    }

    /// Move the focused pane's bordering divider eight device pixels in
    /// `direction`. Use its opposite divider on that axis if needed. Return
    /// whether geometry changed; use a layout from this tree's current geometry.
    pub fn resize(&mut self, direction: Direction, layout: &Layout) -> bool {
        if self.zoomed {
            return false;
        }
        let Some(pane) = layout.pane(self.focused) else {
            return false;
        };
        let axis = direction.axis();
        let forward = direction.forward();
        let Some(divider) = bordering_divider(layout, pane, axis, forward)
            .or_else(|| bordering_divider(layout, pane, axis, !forward))
        else {
            return false;
        };
        let position = if forward {
            divider.position().saturating_add(8)
        } else {
            divider.position().saturating_sub(8)
        };
        self.move_divider(divider, position)
    }

    /// Move a divider to an absolute window-device-pixel position along its
    /// partition axis, keeping at least one pixel on each side. Return whether
    /// geometry changed; use a layout from this tree's current geometry.
    pub fn drag(&mut self, divider_id: u64, position: [u32; 2], layout: &Layout) -> bool {
        if self.zoomed {
            return false;
        }
        let Some(&divider) = layout
            .dividers
            .iter()
            .find(|divider| divider.id == divider_id)
        else {
            return false;
        };
        let [x, y] = position;
        let position = match divider.axis {
            Axis::Horizontal => x,
            Axis::Vertical => y,
        };
        self.move_divider(divider, position)
    }

    /// Replace `layout` while retaining its allocations. Exclusive edges
    /// beyond `u32::MAX` are clipped.
    pub fn layout(&self, bounds: Rect, layout: &mut Layout) {
        layout.panes.clear();
        layout.hidden.clear();
        layout.dividers.clear();
        let bounds = bounds.clipped();
        self.root
            .layout(bounds, layout, self.zoomed.then_some(self.focused));
        if self.zoomed {
            layout.panes.push((self.focused, bounds));
        }
    }

    fn move_divider(&mut self, divider: Divider, position: u32) -> bool {
        let first = position
            .saturating_sub(divider.start)
            .clamp(1, divider.available - 1);
        if first == divider.position() - divider.start {
            return false;
        }
        let Some(branch) = self.root.branch_mut(divider.id) else {
            return false;
        };
        branch.ratio = Ratio {
            first,
            available: divider.available,
        };
        true
    }
}

fn neighbor_score(
    rect: Rect,
    axis: Axis,
    along: Span,
    across: Span,
    forward: bool,
) -> Option<(std::cmp::Reverse<u32>, u64)> {
    if rect.is_empty() {
        return None;
    }
    let gap = along.forward_gap(rect.span(axis), forward)?;
    let span = rect.span(axis.cross());
    let overlap = across.overlap(span);
    (gap <= 1 && overlap != 0).then(|| {
        let distance = across.doubled_center().abs_diff(span.doubled_center());
        (std::cmp::Reverse(overlap), distance)
    })
}

fn bordering_divider(layout: &Layout, pane: Rect, axis: Axis, forward: bool) -> Option<Divider> {
    let along = pane.span(axis);
    let across = pane.span(axis.cross());
    layout.dividers.iter().copied().find(|divider| {
        divider.axis == axis
            && along.forward_gap(divider.rect.span(axis), forward) == Some(0)
            && across.overlap(divider.rect.span(axis.cross())) != 0
    })
}

#[derive(Clone, Copy)]
struct Ratio {
    first: u32,
    available: u32,
}

impl Ratio {
    const EVEN: Self = Self {
        first: 1,
        available: 2,
    };

    fn offset(self, available: u32) -> u32 {
        let offset = u64::from(available) * u64::from(self.first) / u64::from(self.available);
        let offset = u32::try_from(offset).expect("a split fraction never exceeds its span");
        if available >= 2 {
            offset.clamp(1, available - 1)
        } else {
            offset
        }
    }
}

fn partition(rect: Rect, axis: Axis, ratio: Ratio) -> [Rect; 3] {
    let Span { start, len } = rect.span(axis);
    // At tiny sizes, spend pixels on the panes before showing a divider.
    let divider = u32::from(len >= 3 && !rect.is_empty());
    let available = len - divider;
    let first = ratio.offset(available);
    let position = start + first;
    [
        rect.with_span(axis, start, first),
        rect.with_span(axis, position, divider),
        rect.with_span(axis, position + divider, available - first),
    ]
}

struct Branch {
    id: u64,
    axis: Axis,
    ratio: Ratio,
    first: Node,
    second: Node,
}

impl Branch {
    fn layout(&self, rect: Rect, layout: &mut Layout, zoom: Option<u64>) {
        let [first, divider, second] = partition(rect, self.axis, self.ratio);
        if zoom.is_none() && !divider.is_empty() {
            let Span { start, len } = rect.span(self.axis);
            layout.dividers.push(Divider {
                id: self.id,
                rect: divider,
                axis: self.axis,
                start,
                available: len - 1,
            });
        }
        self.first.layout(first, layout, zoom);
        self.second.layout(second, layout, zoom);
    }
}

enum Node {
    Leaf(u64),
    Split(Box<Branch>),
}

impl Node {
    fn focused_rect(&self, id: u64, rect: Rect) -> Rect {
        self.leaf_rect(id, rect)
            .expect("the focused pane is always a leaf in the tree")
    }

    fn leaf_rect(&self, id: u64, rect: Rect) -> Option<Rect> {
        match self {
            Self::Leaf(leaf) => (*leaf == id).then_some(rect),
            Self::Split(branch) => {
                let [first, _, second] = partition(rect, branch.axis, branch.ratio);
                branch
                    .first
                    .leaf_rect(id, first)
                    .or_else(|| branch.second.leaf_rect(id, second))
            }
        }
    }

    fn focused_leaf_mut(&mut self, id: u64) -> &mut Self {
        self.leaf_mut(id)
            .expect("the focused pane is always a leaf in the tree")
    }

    fn leaf_mut(&mut self, id: u64) -> Option<&mut Self> {
        match self {
            Self::Leaf(leaf) => (*leaf == id).then_some(self),
            Self::Split(branch) => branch
                .first
                .leaf_mut(id)
                .or_else(|| branch.second.leaf_mut(id)),
        }
    }

    fn branch_mut(&mut self, id: u64) -> Option<&mut Branch> {
        let Self::Split(branch) = self else {
            return None;
        };
        if branch.id == id {
            Some(branch)
        } else {
            branch
                .first
                .branch_mut(id)
                .or_else(|| branch.second.branch_mut(id))
        }
    }

    fn remove(&mut self, id: u64) -> Option<u64> {
        let Self::Split(branch) = self else {
            return None;
        };
        let (sibling, first) = if matches!(branch.first, Self::Leaf(leaf) if leaf == id) {
            (&mut branch.second, true)
        } else if matches!(branch.second, Self::Leaf(leaf) if leaf == id) {
            (&mut branch.first, false)
        } else {
            return branch.first.remove(id).or_else(|| branch.second.remove(id));
        };
        let replacement = sibling.edge_leaf(first);
        *self = std::mem::replace(sibling, Self::Leaf(id));
        Some(replacement)
    }

    fn edge_leaf(&self, first: bool) -> u64 {
        let mut node = self;
        loop {
            match node {
                Self::Leaf(id) => return *id,
                Self::Split(branch) => node = if first { &branch.first } else { &branch.second },
            }
        }
    }

    fn layout(&self, rect: Rect, layout: &mut Layout, zoom: Option<u64>) {
        match self {
            Self::Leaf(id) if zoom.is_none() => layout.panes.push((*id, rect)),
            Self::Leaf(id) if zoom != Some(*id) => layout.hidden.push((*id, rect)),
            Self::Leaf(_) => (),
            Self::Split(branch) => branch.layout(rect, layout, zoom),
        }
    }
}
