//! Hand the tab labels, activity and progress to the view; a lone tab has no strip, so its
//! progress goes along the window's top edge instead.

use crate::app::Session;

impl Session {
    pub(in crate::app) fn draw_tab_strip(&mut self) {
        let tabs = self.tabs.as_slice();
        if tabs.is_empty() {
            return;
        }
        let active = self.tabs.active_index().expect("live window");
        let snapshot = tabs[active]
            .focused()
            .snapshot
            .as_ref()
            .expect("visible pane snapshot");
        if tabs.len() < 2 {
            self.view
                .draw_edge_progress(tabs[active].progress(), snapshot);
            return;
        }
        self.view.draw_tabs(
            tabs.iter().map(|tab| {
                (
                    tab.focused().label(&self.shell),
                    tab.activity,
                    tab.progress(),
                )
            }),
            active,
            snapshot,
        );
    }
}
