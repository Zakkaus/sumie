"""The negative controls that need macOS: the window gates and the macos-ci build."""
import os
import sys

from negative.probe import append, probe, replace_once

# A default-mode-only performSelector wake. objc2 marks performSelector unsafe, so the
# probe also lifts sumie-macos's unsafe forbid; only this temporary build sees either.
DEFAULT_MODE_WAKE = """    let send = move || {
        let _ = proxy.send_event(event.clone());
    };
    let _ = DEFAULT_MODE_WAKE.set(std::sync::Mutex::new(Box::new(send)));
    || {
        let target: objc2::rc::Retained<DefaultModeWake> =
            unsafe { objc2::msg_send![<DefaultModeWake as objc2::ClassType>::class(), new] };
        let modes = objc2_foundation::NSArray::from_slice(&[objc2_foundation::ns_string!(
            "kCFRunLoopDefaultMode"
        )]);
        let () = unsafe {
            objc2::msg_send![&*target, performSelectorOnMainThread: objc2::sel!(wake),
                withObject: None::<&objc2::runtime::AnyObject>, waitUntilDone: false,
                modes: &*modes]
        };
    }
}

static DEFAULT_MODE_WAKE: std::sync::OnceLock<std::sync::Mutex<Box<dyn Fn() + Send>>> =
    std::sync::OnceLock::new();

objc2::define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    #[thread_kind = objc2::AnyThread]
    #[name = "SumieDefaultModeWake"]
    struct DefaultModeWake;

    impl DefaultModeWake {
        #[unsafe(method(wake))]
        fn wake(&self) {
            (DEFAULT_MODE_WAKE.get().unwrap().lock().unwrap())();
        }
    }
);
"""


def ime_controls():
    """Without a Pinyin input source macos-ime exits 2, so these report "could not run"."""
    ime = "crates/sumie-macos/src/ime.rs"
    late = "ime: late: a late preedit and commit after the switch: recorded e68a8a"
    probe("commit sent twice", "macos-ime", ime,
          replace_once("                if let Some((generation, _)) = self.owner.take() {\n"
                       "                    out.push(Input::Commit",
                       "                if let Some((generation, _)) = self.owner.clone() {\n"
                       "                    out.push(Input::Commit"),
          "ime: commit: once, then not again without a composition: recorded e68a8ae68a8a")
    probe("late commit delivered to the new pane", "macos-ime", ime,
          replace_once("        self.generation = generation;\n        self.owner\n",
                       "        self.generation = generation;\n        if let Some((owner, _)) = &mut self.owner {\n"
                       "            *owner = generation;\n        }\n        self.owner\n"),
          late)
    probe("late preedit binds the new pane", "macos-ime", ime,
          replace_once("        self.generation = generation;\n        self.owner\n",
                       "        self.generation = generation;\n        self.owner = None;\n        self.owner\n"),
          late)
    probe("the native cancel's Disabled leaves the old composition running", "macos-ime", ime,
          replace_once("            Ime::Disabled => out.extend(self.cancel()),\n", "            Ime::Disabled => (),\n"),
          "ime: late: search's composition after the cancel: the overlay shows 0 - - instead of 3 3 3")
    probe("preedit after Cmd+F converted before the chord", "macos-ime", "src/macos/window.rs",
          replace_once("        self.batch\n            .push_back(Queued::Ime(ime, Platform::input_source()));\n",
                       "        let mut events = Vec::new();\n"
                       "        self.convert(Queued::Ime(ime, Platform::input_source()), &mut events);\n"
                       "        self.batch.extend(events.into_iter().map(Queued::Event));\n"),
          "ime: batch: the preedit right after Cmd+F, in search: the overlay shows 0 - - instead of 3 3 3")
    probe("swallowed Kitty release not sent", "macos-ime", ime,
          replace_once("                out.extend(self.release_held());\n", ""),
          "ime: kitty: release when the composition takes over: recorded 1b5b393775 instead of")
    probe("takeover release sent at every preedit", "test", ime,
          replace_once("self.held.drain(..).map(", "self.held.clone().into_iter().map("),
          "test ime::tests::the_composition_releases_held_presses_once ... FAILED")
    probe("real key-up after the takeover release reaches the PTY", "macos-ime", "src/input.rs",
          replace_once("            _ => return false,\n        }\n        true\n",
                       "            (KeyAction::Release, None) => (),\n            _ => return false,\n        }\n"
                       "        true\n"),
          "ime: kitty: one release, then the commit: recorded 1b5b3937751b5b39373b313a33751b5b39373b313a3375")
    probe("takeover release sent to the new recipient", "macos-ime", "src/app/tabs.rs",
          replace_once("        if let Some(id) = self.routing.release_owner(event)\n",
                       "        if let Some(id) = self.routing.release_owner(event).filter(|_| false)\n"),
          "ime: switch: the takeover release after a pane switch reaches pane 1: recorded 1b5b393775 instead of")
    probe("preedit caret dropped before the overlay", "macos-ime", "src/app/pane_input.rs",
          replace_once("            view.preedit_cursor = cursor;\n", "            view.preedit_cursor = None;\n"),
          "ime: commit: preedit: the overlay shows 3 - - instead of 3 3 3")
    probe("commit kept after an input-source change", "test", ime,
          replace_once(".is_some_and(|(_, owner)| *owner != source)", ".is_some_and(|_| false)"),
          "test ime::tests::a_commit_from_another_input_source_is_dropped ... FAILED")
    probe("underline the whole preedit despite its selected clause", "test", "src/view/paint.rs",
          replace_once(".filter(|(start, end)| start < end);", ".filter(|_| false);"),
          "test view::tests::preedit::the_preedit_underline_spans_the_selected_clause ... FAILED")
    probe("search caret pinned to the preedit's end", "test", "src/view/overlays.rs",
          replace_once("if start == end && start < self.preedit.len() =>", "if false =>"),
          "test view::tests::preedit::the_search_caret_follows_a_collapsed_input_method_caret ... FAILED")
    if os.environ.get("SUMIE_IME_NATIVE") == "skip":
        print("negative controls: no native cancel on a recipient change: NOT CHECKED (SUMIE_IME_NATIVE=skip)")
        return
    probe("no native cancel on a recipient change", "macos-ime", "src/macos/window.rs",
          replace_once("            self.platform.cancel_composition();\n", ""),
          "ime: cancel: a pane split: a continued the old composition")


def macos_controls():
    if sys.platform == "darwin":
        probe("pedantic lint in macos-ci code", "clippy-macos-ci", "crates/sumie-macos/src/runner.rs",
              append("#[must_use]\npub fn narrow_probe(value: u16) -> u8 {\n    value as u8\n}\n"),
              "clippy::cast_possible_truncation")
        probe("default-mode-only performSelector wake", "macos-wake", "crates/sumie-macos/src/lib.rs",
              lambda source: replace_once("#![forbid(unsafe_code)]\n", "")(
                  replace_once("    move || {\n        let _ = proxy.send_event(event.clone());\n    }\n}\n",
                               DEFAULT_MODE_WAKE)(source)),
              "macos-wake: event-tracking: no wake inside the 300 ms interval")
        probe("exit watch removed on the first NOTE_EXIT", "test", "src/macos/sources.rs",
              replace_once("        self.take_ready(id).exit\n",
                           "        let exit = self.take_ready(id).exit;\n"
                           "        if exit {\n"
                           "            self.readiness.remove(Key::new(id, Interest::Exit));\n"
                           "        }\n"
                           "        exit\n"),
              "test macos::sources::tests::an_unreaped_exit_keeps_reporting_until_unregistered ... FAILED")
        probe("wake for a removed key delivered", "test", "crates/sumie-macos/src/readiness.rs",
              replace_once("        let live = !state.ready.is_empty();\n", "        let live = true;\n"),
              "test macos::sources::tests::a_report_queued_before_unregister_never_surfaces_after_it ... FAILED")
        probe("no re-watch after an early NOTE_EXIT", "test", "crates/sumie-pty/src/child/macos.rs",
              replace_once("            self.rewatch()?;\n        }\n        Ok(status",
                           "        }\n        Ok(status"),
              "test child::platform::tests::early_note_exit_is_watched_until_reaped ... FAILED")
        probe("frame cap checked after allocation", "test", "src/reload/frame.rs",
              lambda source: replace_once(
                  "        if size > MAX_BODY {\n            return Err(FrameError::TooLong(length));\n"
                  "        }\n        Ok((kind, size))", "        Ok((kind, size))")(
                  replace_once("            self.body = Some((kind, Vec::with_capacity(length), length));\n",
                               "            self.body = Some((kind, Vec::with_capacity(length), length));\n"
                               "            if length > MAX_BODY {\n"
                               "                return Err(FrameError::TooLong(u32::try_from(length).unwrap()));\n"
                               "            }\n")(source)),
              "test reload::frame::tests::a_body_over_the_cap_is_rejected_before_allocation ... FAILED")
        probe("config child waited for in Drop", "test", "src/macos/reload.rs",
              replace_once("    fn drop(&mut self) {\n        if let Some((_, key)) = self.kill.take() {",
                           "    fn drop(&mut self) {\n        let _ = self.child.wait();\n"
                           "        if let Some((_, key)) = self.kill.take() {"),
              "test macos::reload::tests::dropping_a_blocked_child_does_not_wait ... FAILED")
        probe("Core Text bypasses every fallback step", "test", "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("let Some(ch) = ch else {", "let Some(ch) = ch.filter(|_| false) else {"),
              "test tests::core_text_loads_the_named_collection_face_and_cjk_fallback ... FAILED")
        probe("Core Text system-wide fallback queries an uncovered character", "test",
              "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("location: u32::from(ch).try_into().ok()?,", "location: 0x378,"),
              "test tests::core_text_system_wide_fallback_covers_cjk ... FAILED")
        probe("Core Text system-wide fallback runs for undrawable characters", "font-test",
              "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("        if !drawable(ch) {\n", "        if !drawable(ch) && false {\n"),
              "test discovery::core_text::tests::undrawable_misses_skip_the_system_wide_step ... FAILED")
        probe("Core Text system-wide fallback waits for the worker without a bound", "test",
              "crates/sumie-font/src/discovery/core_text.rs",
              replace_once("self.answers.recv_timeout(SYSTEM_WIDE_WAIT)", "self.answers.recv()"),
              "test discovery::core_text::tests::a_slow_system_wide_lookup_is_pending_after_the_wait ... FAILED")
        login = "test macos::login::tests::"
        lsof = login + "a_login_shell_inherits_no_parent_descriptor ... "
        probe("bridge duplicate inherited across exec", "test", "crates/sumie-macos/src/readiness.rs",
              replace_once("        let fd = fd.try_clone_to_owned()?;\n",
                           "        let fd = fd.try_clone_to_owned()?;\n"
                           "        fcntl(&fd, FcntlArg::F_SETFD(FdFlag::empty()))?;\n"),
              lsof + "FAILED")
        # XNU never copies a kqueue into a child, so the lsof test stays green here; the flag
        # test catches it, and the duplicates above are what the bridge could leak.
        probe("bridge kqueue without FD_CLOEXEC", "test", "crates/sumie-macos/src/readiness.rs",
              replace_once("fcntl(&queue, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;",
                           "fcntl(&queue, FcntlArg::F_SETFD(FdFlag::empty()))?;"),
              "test readiness::tests::bridge_descriptors_close_on_exec ... FAILED")
        probe("PS0 exported to Bash 3.2", "test", "shell-integration/bash/sumie.bash",
              replace_once("\n_sumie_ran=0\n", "\n_sumie_ran=0\nexport PS0='\\e]133;C\\a'\n"),
              login + "bash_3_2_login_shell_reads_its_profile_once_and_marks_prompts ... FAILED")
        probe("Bash 3.2 bootstrap no-op resets $?", "test", "shell-integration/bash/sumie.bash",
              replace_once("""//"$_sumie_boot"/'${_sumie_ran+_sumie_keep}'}""", """//"$_sumie_boot"/:}"""),
              login + "bash::bash_3_2_profile_appends_after_the_bootstrap ... FAILED")
        probe("Bash 3.2 DEBUG trap not reinstalled", "test", "shell-integration/bash/sumie.bash",
              replace_once('|| eval "${_sumie_trap-}"', "|| :"),
              login + "bash::bash_3_2_reinstalls_its_trap_after_one_set_at_the_prompt ... FAILED")
        probe("stop forwarding keys", "macos-keys", "src/macos/window.rs",
              replace_once("            Native::Key(key) => self.batch.push_back(Queued::Key(key)),\n",
                           "            Native::Key(_) => (),\n"),
              "keys: plain: recorded nothing instead of")
        probe("report focus on ModifiersChanged", "macos-keys", "src/app/pane_input.rs",
              replace_once("            pane.pane.pointer_modifiers_changed(modifiers);\n",
                           "            pane.pane.pointer_modifiers_changed(modifiers);\n"
                           "            if modifiers == KeyModifiers::default() {\n"
                           "                input::report_focus(&mut pane.pane, false)?;\n"
                           "            }\n"),
              "keys: focus: 2 focus-out reports for one focus loss")
        probe("leak the release of a consumed shortcut", "macos-keys", "src/input.rs",
              replace_once("                Some(Shortcut::Consume) => return Ok(Effect::None),\n",
                           "                Some(Shortcut::Consume) if !matches!(action, KeyAction::Release) => {\n"
                           "                    return Ok(Effect::None);\n"
                           "                }\n"
                           "                Some(Shortcut::Consume) => (),\n"),
              "keys: command chords: recorded")
        probe("repeat a key held into Command with it", "macos-keys", "crates/sumie-core/src/shortcuts.rs",
              replace_once("                .then_some(Shortcut::Consume));\n",
                           "                .then_some(Shortcut::Consume).filter(|_| false));\n"),
              "keys: a held into Command: recorded")
        probe("right Option sent as Meta", "macos-keys", "crates/sumie-macos/src/window.rs",
              replace_once("        right_alt: state.alt_key() && !left_alt,\n",
                           "        right_alt: state.alt_key() && !left_alt && false,\n"),
              "keys: option: right Option: recorded")
        probe("Option without a side sent as Meta", "macos-keys", "crates/sumie-macos/src/window.rs",
              replace_once("let left_alt = modifiers.lalt_state() == ModifiersKeyState::Pressed;",
                           "let left_alt = modifiers.ralt_state() != ModifiersKeyState::Pressed;"),
              "keys: option: Option without a side: recorded")
        # The unused macos_shortcut is only a warning, so the build runs and left Option+1
        # selects a tab instead of typing ESC 1.
        probe("Linux Alt+digit tab aliases on macOS", "macos-keys", "crates/sumie-core/src/shortcuts.rs",
              replace_once("            Platform::MacOS => macos_shortcut(physical, modifiers, has_splits),\n",
                           "            Platform::MacOS => linux_shortcut(physical, symbol, modifiers, has_splits),\n"),
              "keys: option: left Option: recorded 1b61 instead of 1b611b31")
        probe("Cmd+Z zooms the pane", "macos-keys", "crates/sumie-core/src/shortcuts.rs",
              replace_once("        'w' => Shortcut::Tab(TabAction::Close),\n",
                           "        'w' => Shortcut::Tab(TabAction::Close),\n"
                           "        'z' => Shortcut::Pane(PaneAction::Zoom),\n"),
              "keys: zoom: Cmd+Z resized the focused pane")
        probe("Cmd+Shift+Return unbound", "macos-keys", "crates/sumie-core/src/shortcuts.rs",
              replace_once("    if modifiers.shift && physical == PhysicalKey::Code(KeyCode::Enter) {\n",
                           "    if false && modifiers.shift && physical == PhysicalKey::Code(KeyCode::Enter) {\n"),
              "keys: zoom: after Cmd+Shift+Return: the focused pane stayed")
        probe("open a link released after a drag off it", "macos-keys", "crates/sumie-core/src/mouse.rs",
              replace_once("            self.link_press = None;\n            self.selecting = Some((SelectionUnit::Cell, cell));\n",
                           "            self.selecting = Some((SelectionUnit::Cell, cell));\n"),
              "keys: links: a Cmd drag onto the second and back opened URIs of lengths [21]")
        probe("cancel a Cmd+click that moved along its link", "macos-keys", "crates/sumie-core/src/lib.rs",
              replace_once("        matches!(self, Self::MacOS)\n", "        false\n"),
              "keys: links: a Cmd drag along the first opened URIs of lengths none")
        probe("a tracking drag into the padding beside its cell is a click", "test", "crates/sumie-core/src/mouse.rs",
              replace_once("    event.over_grid && event.cell == cell\n", "    event.cell == cell\n"),
              "test mouse::tests::a_held_back_press_is_a_click_only_inside_its_cell ... FAILED")
        probe("a held-back click after another button's press and release", "test", "crates/sumie-core/src/mouse.rs",
              replace_once("        if !left {\n            self.held_back = None;\n        }\n", ""),
              "test mouse::tests::another_button_drops_a_held_back_press ... FAILED")
        probe("report a tracking press before it becomes a click or a drag", "macos-keys",
              "crates/sumie-core/src/mouse.rs",
              replace_once("                if left && modes.tracking && !event.modifiers.alt && event.over_grid =>\n",
                           "                if false =>\n"),
              "keys: tracking: recorded")
        probe("copy on a drag release", "macos-pasteboard", "crates/sumie-core/src/lib.rs",
              replace_once("        matches!(self, Self::Linux)\n", "        true\n"),
              "pasteboard: copy: the drag release changed the sentinel")
        # A unit test, so no check ever serves the general board.
        probe("select the general board in a macos-ci build", "test", "src/macos/pasteboard.rs",
              replace_once("    Ok(Board::named(name)?)\n}", "    let _ = name;\n    Ok(Board::General)\n}"),
              "test macos::pasteboard::tests::a_ci_build_serves_only_a_named_board ... FAILED")
        probe("start a helper for every repeated paste", "test", "src/macos/pasteboard.rs",
              replace_once("            && self.reading(recipient)\n", "            && self.reading(recipient) && false\n"),
              "test macos::pasteboard::tests::a_second_paste_for_the_same_recipient_joins_the_pending_one ... FAILED")
        # A unit test: whether the end-to-end stale case sees the reply arrive after the next
        # request depends on timing it cannot control.
        probe("stamp a paste with the current generation", "test", "src/macos/window/native.rs",
              replace_once("        generation: recipient,\n", "        generation: current,\n"),
              "test macos::window::native::tests::a_paste_keeps_its_request_generation ... FAILED")
        probe("send a paste unframed when the framed one does not fit", "macos-pasteboard", "src/pane.rs",
              replace_once("                self.pty.enqueue_input(&self.terminal.encode_paste(text)?)?;\n",
                           "                let unframed = text.clone().into_bytes();\n"
                           "                if self.pty.enqueue_input(&self.terminal.encode_paste(text)?).is_err() {\n"
                           "                    self.pty.enqueue_input(&unframed)?;\n"
                           "                }\n"),
              "pasteboard: oversized: recorded")
        probe("no pasteboard helper timeout", "macos-pasteboard", "src/macos/pasteboard.rs",
              replace_once("const TIMEOUT: Duration = Duration::from_secs(5);",
                           "const TIMEOUT: Duration = Duration::from_secs(3600);"),
              "pasteboard: stalled: the helper outlived its 5 s timeout")
        probe("OSC 52 write without focus", "macos-pasteboard", "src/macos/window.rs",
              replace_once("        if !self.focused {\n", "        if false && !self.focused {\n"),
              "pasteboard: osc52: a write without focus changed the board")
        ime_controls()
        probe("configure on Resized", "macos-resize", "src/app/window_events.rs",
              replace_once("session.view.request_resize(scale, logical, device);\n",
                           "session.view.request_resize(scale, logical, device);\n"
                           "                    self.gpu.resize(device);\n"),
              "configures ran while GPU work was pending")
        probe("present every turn even when clean", "macos-idle", "src/macos/window.rs",
              replace_once("    fn background_commit_needed(&self) -> bool {\n        false",
                           "    fn background_commit_needed(&self) -> bool {\n        true"),
              "presents within")
        probe("retry 50 ms after a drawable timeout", "macos-timeout", "src/macos/surface.rs",
              replace_once("        self.hold(now + wait)\n",
                           "        self.hold(now + wait.min(Duration::from_millis(50)))\n"),
              "timeout: acquired again")
        probe("free the frame slot on every wake", "macos-flood", "src/macos/driver.rs",
              replace_once("                open.window.watches.dispatch()?;\n",
                           "                open.window.watches.dispatch()?;\n"
                           "                crate::platform::WindowOps::recover_frame_slot(&mut open.window);\n"),
              "flood: more than one present per refresh")
        probe("wait for the first settings without a deadline", "macos-loading", "src/macos/driver.rs",
              replace_once("None if Instant::now() >= *first_by =>", "None if false && Instant::now() >= *first_by =>"),
              "loading: no present within")
        probe("discard the hot-reload diagnostic", "macos-loading", "src/app/window_events.rs",
              replace_once("if show_error(&mut self.banner, message)",
                           "if !message.is_empty() && false"),
              "loading: no diagnostic frame after saving an invalid config")
        probe("keep the window visible until the reap", "macos-close", "src/macos/driver.rs",
              replace_once("        open.window\n            .platform()\n            .order_out(context, move || done.set(true));\n",
                           "        std::mem::forget(open.window.platform().window_owner());\n"
                           "        let _ = context;\n"
                           "        done.set(true);\n"),
              "close: the window was still on screen when the child was reaped")
        probe("no SIGKILL at the grace period", "macos-close", "src/app/lifetime.rs",
              replace_once("if watches.take_exit_ready(*id) || pty.close_deadline().is_some_and(|at| at <= now) {",
                           "if watches.take_exit_ready(*id) {"),
              "close: no SIGKILL at the 3 s grace period")
        probe("answer Cancel to a system quit", "macos-system", "crates/sumie-macos/src/lib.rs",
              replace_once("                context.finish_quit();\n",
                           "                context.run_after_handler(|| {\n"
                           "                    objc2_app_kit::NSApplication::sharedApplication(\n"
                           "                        objc2::MainThreadMarker::new().unwrap(),\n"
                           "                    )\n"
                           "                    .replyToApplicationShouldTerminate(false);\n"
                           "                });\n"),
              "system: the watchdog ended the process")
        probe("no watchdog", "macos-stuck", "crates/sumie-macos/src/termination.rs",
              replace_once(".spawn(move || watch(&watched))?", ".spawn(move || drop(watched))?"),
              "stuck: still running 10.5 s after the quit")
        # Its marker is a rate, which hosted runners only record.
        if os.environ.get("SUMIE_TIMING") != "record":
            probe("an 8-byte PTY read buffer", "macos-throughput", "crates/sumie-pty/src/reader.rs",
                  replace_once("const CHUNK_SIZE: usize = 8192;", "const CHUNK_SIZE: usize = 8;"),
                  "(below 20 MB/s)")
