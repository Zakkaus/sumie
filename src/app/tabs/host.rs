use std::path::Path;

use sumie_core::{Selection, splits::Rect};

use super::SnapshotPace;
use crate::app::{Launch, LocalPane, PaneState, Result, View, directory};
use crate::login::Environment;
use crate::platform::PaneWatches;

pub(in crate::app) struct PaneHost {
    environment: Environment,
    click_interval: Option<u32>,
    image_dimension_limit: u32,
    primary_selection: bool,
}

impl PaneHost {
    pub(in crate::app) const fn new(
        environment: Environment,
        image_dimension_limit: u32,
        click_interval: Option<u32>,
        primary_selection: bool,
    ) -> Self {
        Self {
            environment,
            click_interval,
            image_dimension_limit,
            primary_selection,
        }
    }

    pub(in crate::app) fn open(
        &self,
        (id, rect): (u64, Rect),
        view: &View,
        config: &sumie_config::Config,
        cwd: &Path,
        launch: Launch,
        watches: &mut impl PaneWatches,
    ) -> Result<PaneState> {
        let size = view.pane_size(rect);
        let mut pane = match launch {
            Launch::Shell => LocalPane::login(&self.environment, size, view.cell, config, cwd)?,
            Launch::Settings => LocalPane::settings(
                std::env::current_exe()?.as_os_str(),
                &self.environment,
                size,
                view.cell,
                config,
                cwd,
            )?,
        };
        pane.terminal
            .set_image_dimension_limit(self.image_dimension_limit);
        if let Some(millis) = self.click_interval {
            pane.set_click_interval(millis);
        }
        pane.set_primary_selection(self.primary_selection);
        watches.register_pane(
            id,
            pane.pty.output_fd(),
            pane.pty.exit_fd().expect("new child watcher"),
        )?;
        Ok(PaneState {
            id,
            rect,
            launch,
            cell: view.cell,
            viewport_px: [size.width_px, size.height_px],
            exited: false,
            text_recipient: None,
            search: None,
            viewer: None,
            selection: Selection::default(),
            selected_text: String::new(),
            pane,
            directory: directory::Directory::at_spawn(cwd),
            title: String::new(),
            snapshot: None,
            snapshot_dirty: true,
            snapshot_pace: SnapshotPace::default(),
            frame_dirty: true,
            activity_after: None,
            hover: crate::app::links::Hover::default(),
            scroll: crate::app::scroll::PaneScroll::default(),
            progress: None,
        })
    }
}
