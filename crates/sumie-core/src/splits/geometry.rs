//! Subtree pixel budgets, divider bounds, and split previews.

use super::{Axis, Branch, Node, Rect, Span};

#[derive(Clone, Copy)]
pub(super) struct Ratio {
    pub(super) first: u32,
    pub(super) available: u32,
}

impl Ratio {
    pub(super) const EVEN: Self = Self {
        first: 1,
        available: 2,
    };

    fn offset(self, available: u32) -> u32 {
        let offset = u64::from(available) * u64::from(self.first) / u64::from(self.available);
        u32::try_from(offset).expect("a split fraction never exceeds its span")
    }
}

pub(super) fn combined_tracks(axis: Axis, mut first: [u32; 2], second: [u32; 2]) -> [u32; 2] {
    let along = axis as usize;
    let across = axis.cross() as usize;
    first[along] = first[along].saturating_add(second[along]);
    first[across] = first[across].max(second[across]);
    first
}

pub(super) struct Partition {
    pub(super) rects: [Rect; 3],
    pub(super) limits: [u32; 2],
}

pub(super) fn partition(
    rect: Rect,
    axis: Axis,
    ratio: Ratio,
    tracks: [u32; 2],
    cell: u32,
) -> Partition {
    let Span { start, len } = rect.span(axis);
    let (divider, limits) = budget(rect, len, tracks, cell);
    let available = len - divider;
    let first = ratio.offset(available).clamp(limits[0], limits[1]);
    let position = start + first;
    Partition {
        rects: [
            rect.with_span(axis, start, first),
            rect.with_span(axis, position, divider),
            rect.with_span(axis, position + divider, available - first),
        ],
        limits,
    }
}

fn budget(rect: Rect, len: u32, tracks: [u32; 2], cell: u32) -> (u32, [u32; 2]) {
    let [first, second] = tracks;
    let Some(total) = first.checked_add(second).filter(|&total| total <= len) else {
        // Give pixels to both children when possible, without oversizing either subtree.
        let both = u32::from(len > 1);
        return (
            0,
            [len.saturating_sub(second).max(both), first.min(len - both)],
        );
    };

    // Preserve the same cell floor across both subtrees before spending pixels on chrome.
    let cell = cell.max(1);
    let (floor, slack) = match total.checked_mul(cell) {
        Some(pixels) if pixels <= len => (cell, len - pixels),
        _ => (len / total, len % total),
    };
    let divider = u32::from(slack != 0 && !rect.is_empty());
    let internal_divider = u32::from(divider != 0 && slack >= total - 1);
    let [first, second] = tracks.map(|count| count * floor + internal_divider * (count - 1));
    (divider, [first, len - divider - second])
}

pub(super) fn split_preview(
    root: &Node,
    focused: u64,
    axis: Axis,
    bounds: Rect,
    cell: [u32; 2],
) -> Rect {
    let preview = Preview {
        focused,
        tracks: combined_tracks(axis, [1; 2], [1; 2]),
        bounds,
        cell,
    };
    let pane = preview
        .find(root, None)
        .expect("the focused pane is always a leaf in the tree");
    partition(pane, axis, Ratio::EVEN, [1; 2], cell[axis as usize]).rects[2]
}

struct Preview {
    focused: u64,
    tracks: [u32; 2],
    bounds: Rect,
    cell: [u32; 2],
}

impl Preview {
    fn find(&self, node: &Node, parent: Option<&PreviewStep<'_>>) -> Option<Rect> {
        match node {
            Node::Leaf(id) => (*id == self.focused)
                .then(|| parent.map_or(self.bounds, |step| step.rect(self, self.tracks))),
            Node::Split(branch) => {
                let mut step = PreviewStep {
                    parent,
                    branch,
                    first: true,
                };
                self.find(&branch.first, Some(&step)).or_else(|| {
                    step.first = false;
                    self.find(&branch.second, Some(&step))
                })
            }
        }
    }
}

// Follow the focused path back to the root with the new track counts, then
// partition on the way back down without mutating the tree.
struct PreviewStep<'a> {
    parent: Option<&'a Self>,
    branch: &'a Branch,
    first: bool,
}

impl PreviewStep<'_> {
    fn rect(&self, preview: &Preview, child_tracks: [u32; 2]) -> Rect {
        let branch = self.branch;
        let (first, second) = if self.first {
            (child_tracks, branch.second.tracks())
        } else {
            (branch.first.tracks(), child_tracks)
        };
        let rect = self.parent.map_or(preview.bounds, |parent| {
            parent.rect(preview, combined_tracks(branch.axis, first, second))
        });
        let axis = branch.axis as usize;
        let tracks = [first[axis], second[axis]];
        let parts = partition(rect, branch.axis, branch.ratio, tracks, preview.cell[axis]).rects;
        parts[if self.first { 0 } else { 2 }]
    }
}
