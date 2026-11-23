/// A tab command from the keyboard or window chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabAction {
    Open,
    Close,
    /// Switch by a signed number of tabs, wrapping at each end.
    Cycle(isize),
    /// Select a zero-based position in the current tab order.
    Select(usize),
    Last,
    MoveLeft,
    MoveRight,
}

/// Tab order and active index in one window. Each tab owns its pane payload.
pub struct WindowTabs<T> {
    tabs: Vec<T>,
    active: usize,
}

impl<T> WindowTabs<T> {
    #[must_use]
    pub fn new(first: T) -> Self {
        Self {
            tabs: vec![first],
            active: 0,
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.tabs
    }

    /// Borrow tabs in order. Use [`Self::move_active`] to reorder them.
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.tabs
    }

    #[must_use]
    pub fn active(&self) -> Option<&T> {
        self.tabs.get(self.active)
    }

    pub fn active_mut(&mut self) -> Option<&mut T> {
        self.tabs.get_mut(self.active)
    }

    #[must_use]
    pub fn active_index(&self) -> Option<usize> {
        (!self.tabs.is_empty()).then_some(self.active)
    }

    /// Append a tab and make it active.
    pub fn open(&mut self, tab: T) {
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    /// Preserve the active tab unless closing it. Then select the next tab,
    /// or the previous tab if the closed tab was last.
    pub fn close(&mut self, index: usize) -> Option<T> {
        if index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(index);
        if index < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        Some(tab)
    }

    /// Activate a tab by position. Return whether the active tab changed.
    pub fn switch(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        self.active = index;
        true
    }

    /// Move the active tab to a new position without changing its identity.
    /// Return whether the order changed.
    pub fn move_active(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() || index == self.active {
            return false;
        }
        if index < self.active {
            self.tabs[index..=self.active].rotate_right(1);
        } else {
            self.tabs[self.active..=index].rotate_left(1);
        }
        self.active = index;
        true
    }

    /// Apply a navigation or reorder command. Switching wraps; moving stops at
    /// either edge. Return whether the active tab or order changed.
    /// Opening and closing use the separate tab lifecycle methods.
    pub fn navigate(&mut self, action: TabAction) -> bool {
        let Some(active) = self.active_index() else {
            return false;
        };
        let len = self.tabs.len();
        match action {
            TabAction::Cycle(steps) => {
                let offset = steps.unsigned_abs() % len;
                let forward = if steps < 0 { len - offset } else { offset };
                let remaining = len - active;
                self.switch(if forward < remaining {
                    active + forward
                } else {
                    forward - remaining
                })
            }
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
        use TabAction::{Cycle, Last, MoveLeft, MoveRight, Select};

        let mut tabs = three_tabs();
        for (action, changed, order, index) in [
            (Cycle(1), true, [1, 2, 3], 0),
            (Cycle(-1), true, [1, 2, 3], 2),
            (Cycle(3), false, [1, 2, 3], 2),
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
