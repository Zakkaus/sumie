/// A tab command from the keyboard or window chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAction {
    Open,
    Close,
    Next,
    Previous,
    /// Select a zero-based position in the current tab order.
    Select(usize),
    Last,
    MoveLeft,
    MoveRight,
}

/// Ordered panes in one window. The caller supplies any stable pane identity.
/// Every non-empty window has exactly one active pane.
pub struct WindowTabs<P> {
    panes: Vec<P>,
    active: usize,
}

impl<P> WindowTabs<P> {
    #[must_use]
    pub fn new(first: P) -> Self {
        Self {
            panes: vec![first],
            active: 0,
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[P] {
        &self.panes
    }

    /// Borrow pane payloads in tab order. Use [`Self::move_active`] to reorder them.
    pub fn as_mut_slice(&mut self) -> &mut [P] {
        &mut self.panes
    }

    #[must_use]
    pub fn active(&self) -> Option<&P> {
        self.panes.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut P> {
        self.panes.get_mut(self.active)
    }

    #[must_use]
    pub fn active_index(&self) -> Option<usize> {
        (!self.panes.is_empty()).then_some(self.active)
    }

    /// Append a pane and make it active.
    pub fn open(&mut self, pane: P) {
        self.panes.push(pane);
        self.active = self.panes.len() - 1;
    }

    /// Preserve the active pane unless closing it. Then select the next pane,
    /// or the previous pane if the closed pane was last.
    pub fn close(&mut self, index: usize) -> Option<P> {
        if index >= self.panes.len() {
            return None;
        }
        let pane = self.panes.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.panes.len().saturating_sub(1));
        Some(pane)
    }

    /// Activate a pane by position. Return whether the active pane changed.
    pub fn switch(&mut self, index: usize) -> bool {
        if index >= self.panes.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Move the active pane to a new position without changing its identity.
    /// Return whether the order changed.
    pub fn move_active(&mut self, index: usize) -> bool {
        if index >= self.panes.len() || index == self.active {
            return false;
        }
        if index < self.active {
            self.panes[index..=self.active].rotate_right(1);
        } else {
            self.panes[self.active..=index].rotate_left(1);
        }
        self.active = index;
        true
    }

    /// Apply a navigation or reorder command. Switching wraps; moving stops at
    /// either edge. Return whether the active pane or order changed.
    /// Opening and closing use the separate pane lifecycle methods.
    pub fn navigate(&mut self, action: TabAction) -> bool {
        let Some(active) = self.active_index() else {
            return false;
        };
        let len = self.panes.len();
        match action {
            TabAction::Next => {
                let next = active + 1;
                self.switch(if next == len { 0 } else { next })
            }
            TabAction::Previous => self.switch(if active == 0 { len - 1 } else { active - 1 }),
            TabAction::Select(index) => self.switch(index),
            TabAction::Last => self.switch(len - 1),
            TabAction::MoveLeft => self.move_active(active.saturating_sub(1)),
            TabAction::MoveRight => self.move_active(active + 1),
            TabAction::Open | TabAction::Close => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TabAction, WindowTabs};

    fn three_tabs() -> WindowTabs<u8> {
        let mut tabs = WindowTabs::new(1);
        tabs.open(2);
        tabs.open(3);
        tabs
    }

    #[test]
    fn close_preserves_active_identity_or_selects_a_neighbor() {
        let mut tabs = three_tabs();
        assert_eq!(tabs.active_index(), Some(2));
        for (index, closed, order, active) in [
            (0, Some(1), &[2, 3][..], Some(1)),
            (2, None, &[2, 3], Some(1)),
            (1, Some(3), &[2], Some(0)),
            (0, Some(2), &[], None),
        ] {
            assert_eq!(tabs.close(index), closed);
            assert_eq!(tabs.as_slice(), order);
            assert_eq!(tabs.active_index(), active);
        }
        tabs.open(4);
        tabs.open(5);
        assert!(tabs.switch(0));
        assert_eq!(tabs.close(0), Some(4));
        assert_eq!(tabs.active(), Some(&5));
    }

    #[test]
    fn navigation_wraps_but_reorder_stops_at_edges() {
        use TabAction::{Last, MoveLeft, MoveRight, Next, Previous, Select};

        let mut tabs = three_tabs();
        for (action, changed, order, index) in [
            (Next, true, [1, 2, 3], 0),
            (Previous, true, [1, 2, 3], 2),
            (MoveRight, false, [1, 2, 3], 2),
            (MoveLeft, true, [1, 3, 2], 1),
            (MoveRight, true, [1, 2, 3], 2),
            (Select(0), true, [1, 2, 3], 0),
            (MoveLeft, false, [1, 2, 3], 0),
            (Select(3), false, [1, 2, 3], 0),
            (Last, true, [1, 2, 3], 2),
        ] {
            assert_eq!(tabs.navigate(action), changed);
            assert_eq!(tabs.as_slice(), order.as_slice());
            assert_eq!(tabs.active_index(), Some(index));
            assert_eq!(tabs.active(), Some(&order[index]));
        }
    }
}
