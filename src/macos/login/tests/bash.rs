//! Bash 3.2 keeps its marks however the profile edits `PROMPT_COMMAND` and the DEBUG trap.

use super::*;

const PROFILE: &str = r#"PS1='[$?]\$ '
user_hook() { echo "hook:$?" >> "$HOME/hook.log"; }
"#;

/// Run a `/bin/bash` login whose profile ends with `edit`, and check that every command
/// (but the first unless `first_marked`) is marked, `hook` reaches the hook log with the
/// command's status, and no bootstrap text is left.
fn assert_marks(
    test: &str,
    env: &[(&str, &str)],
    edit: &str,
    hook: Option<&str>,
    first_marked: bool,
) -> Option<(Home, Session)> {
    let mut vars = vec![("SHELL", "/bin/bash")];
    vars.extend_from_slice(env);
    let home = isolated(module_path!(), test, &vars)?;
    home.write(".bash_profile", &format!("{PROFILE}{edit}\n"));
    home.write(".bashrc", "PS1='nested$ '\n");
    let mut session = Session::login(&home);
    session.until("\x1b]133;A\x07");
    session.until("[0]$ \x1b]133;B\x07");
    if first_marked {
        assert_command(&mut session, "echo first", 0, "[0]$ ");
    } else {
        session.send("echo first\r");
        let unmarked = session.until("[0]$ \x1b]133;B\x07");
        assert!(!unmarked.contains("133;C"), "{unmarked:?}");
    }
    assert_command(&mut session, "false", 1, "[1]$ ");
    assert_empty_line(&mut session, "[1]$ ");
    let output = assert_command(
        &mut session,
        r#"[[ $PROMPT_COMMAND == *SUMIE_BASH_SCRIPT* ]]; echo "boot-left=$?""#,
        0,
        "[0]$ ",
    );
    assert!(output.contains("boot-left=1"), "{output:?}");
    assert_clean(&session);
    if let Some(hook) = hook {
        let log = home.read("hook.log");
        assert!(log.contains(&format!("{hook}:1\n")), "hook log {log:?}");
    }
    Some((home, session))
}

#[test]
fn bash_3_2_profile_appends_after_the_bootstrap() {
    assert_marks(
        "bash_3_2_profile_appends_after_the_bootstrap",
        &[],
        r#"PROMPT_COMMAND="$PROMPT_COMMAND; user_hook""#,
        Some("hook"),
        true,
    );
}

#[test]
fn bash_3_2_profile_prepends_before_the_bootstrap() {
    assert_marks(
        "bash_3_2_profile_prepends_before_the_bootstrap",
        &[],
        r#"PROMPT_COMMAND="user_hook; $PROMPT_COMMAND""#,
        Some("hook"),
        true,
    );
}

#[test]
fn bash_3_2_keeps_a_user_command_that_contains_late() {
    assert_marks(
        "bash_3_2_keeps_a_user_command_that_contains_late",
        &[],
        r#"PROMPT_COMMAND="$PROMPT_COMMAND; echo \"latest:\$?\" >> \"\$HOME/hook.log\"""#,
        Some("latest"),
        true,
    );
}

#[test]
fn bash_3_2_runs_the_requested_prompt_command_and_a_profile_append() {
    assert_marks(
        "bash_3_2_runs_the_requested_prompt_command_and_a_profile_append",
        &[("PROMPT_COMMAND", "user_hook")],
        r#"PROMPT_COMMAND="$PROMPT_COMMAND; :""#,
        Some("hook"),
        true,
    );
}

/// Like bash-preexec at its first prompt: clear the DEBUG trap at top level, install one
/// from a function that runs the replaced trap only for interactive commands, and append an
/// entry after sumie's prompt hook.
#[test]
fn bash_3_2_chains_a_debug_trap_installed_at_the_first_prompt() {
    let edit = r#"install='replaced=$(trap -p DEBUG); trap - DEBUG; install_once'
preexec() {
    [[ -n ${interactive-} ]] || return 0
    interactive=
    echo "debug:$BASH_COMMAND" >> "$HOME/debug.log"
    eval "$prior"
}
install_once() {
    prior=${replaced#trap -- \'}
    prior=${prior%\' DEBUG}
    trap preexec DEBUG
    PROMPT_COMMAND=${PROMPT_COMMAND//"$install"/:}$'\n'user_hook$'\n'interactive=1
}
PROMPT_COMMAND="$PROMPT_COMMAND"$'\n'"$install""#;
    // Sumie's trap is back from the second prompt.
    let Some((home, _session)) = assert_marks(
        "bash_3_2_chains_a_debug_trap_installed_at_the_first_prompt",
        &[],
        edit,
        Some("hook"),
        false,
    ) else {
        return;
    };
    let debug = home.read("debug.log");
    assert!(
        debug.contains("debug:false\n"),
        "the later DEBUG trap lost: {debug:?}"
    );
}

#[test]
fn a_nested_bash_runs_nothing_of_sumie() {
    let Some((_home, mut session)) = assert_marks(
        "a_nested_bash_runs_nothing_of_sumie",
        &[],
        "saved_prompt_command=$PROMPT_COMMAND",
        None,
        true,
    ) else {
        return;
    };
    for nested in [
        "export PROMPT_COMMAND; bash -i",
        // The bootstrap entry itself, inherited without SUMIE_BASH_SCRIPT.
        "PROMPT_COMMAND=$saved_prompt_command bash -i",
    ] {
        session.send(&format!("{nested}\r"));
        session.until("nested$ ");
        session.send("echo \"nested=$?\"\r");
        session.until("nested=0");
        let prompt = session.until("nested$ ");
        assert!(
            !prompt.contains("\x1b]"),
            "{nested}: nested prompt {prompt:?}"
        );
        session.send("exit\r");
        session.until("\x1b]133;D;0\x07");
        session.until("[0]$ \x1b]133;B\x07");
    }
    assert_clean(&session);
}
