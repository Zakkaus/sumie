use std::cell::Cell;

use session::{Descriptors, Home, Session, isolated, osc7_host};

use crate::platform::PaneWatches as _;

use super::*;

mod bash;
mod session;

const NOT_A_LOCALE: &str = "xx_YY.UTF-8";

fn no_account() -> Result<Option<PathBuf>> {
    panic!("the requested shell was usable")
}

fn chosen(requested: Option<&str>, account: Option<&str>) -> Result<PathBuf> {
    let requested = requested.map(OsString::from);
    shell(requested.as_ref(), || Ok(account.map(PathBuf::from)))
}

#[test]
fn an_absolute_executable_shell_is_kept() {
    let requested = OsString::from("/bin/zsh");
    assert_eq!(
        shell(Some(&requested), no_account).unwrap(),
        Path::new("/bin/zsh")
    );
}

#[test]
fn an_unusable_shell_falls_back_to_the_account() {
    for requested in [
        None,
        Some("bash"),
        Some("/no/such/sh"),
        Some("/bin"),
        Some("/etc/hosts"),
    ] {
        assert_eq!(
            chosen(requested, Some("/bin/zsh")).unwrap(),
            Path::new("/bin/zsh"),
            "{requested:?}"
        );
    }
}

#[test]
fn without_any_usable_shell_login_fails() {
    for account in [None, Some("zsh"), Some("/no/such/sh")] {
        assert!(chosen(Some("bash"), account).is_err(), "{account:?}");
    }
    let asked = Cell::new(false);
    let result = shell(None, || {
        asked.set(true);
        Err("no user database".into())
    });
    assert!(asked.get() && result.is_err());
}

#[test]
fn a_utf8_locale_is_added_unless_one_is_in_effect() {
    let exists = |name: &str| name != NOT_A_LOCALE && locale_exists(name);
    for (vars, added) in [
        (&[][..], Some("en_US.UTF-8")),
        (&[("LANG", "")][..], Some("en_US.UTF-8")),
        (&[("LANG", NOT_A_LOCALE)][..], Some("en_US.UTF-8")),
        (&[("LC_CTYPE", "../../etc")][..], Some("en_US.UTF-8")),
        (&[("LC_CTYPE", ".UTF-8")][..], Some("en_US.UTF-8")),
        (&[("LANG", "C")][..], Some("en_US.UTF-8")),
        (&[("LANG", "POSIX")][..], Some("en_US.UTF-8")),
        (&[("LANG", "fr_FR.ISO8859-1")][..], Some("en_US.UTF-8")),
        (&[("LANG", "fr_FR.UTF-8")][..], None),
        (
            &[("LC_CTYPE", "C"), ("LANG", "fr_FR.UTF-8")][..],
            Some("en_US.UTF-8"),
        ),
        (&[("LC_CTYPE", "UTF-8"), ("LANG", NOT_A_LOCALE)][..], None),
        (
            &[("LC_CTYPE", NOT_A_LOCALE), ("LANG", "fr_FR.UTF-8")][..],
            Some("en_US.UTF-8"),
        ),
        (&[("LC_ALL", "C")][..], None),
        (&[("LC_ALL", NOT_A_LOCALE)][..], None),
    ] {
        let env = vars
            .iter()
            .map(|&(key, value)| (key.into(), value.into()))
            .collect();
        assert_eq!(missing_locale(&env, exists), added, "{vars:?}");
    }
}

/// Startup files that log each read and define a command only a profile defines.
fn bash_profile(home: &Home) {
    home.write(
        ".bash_profile",
        r#"echo profile >> "$HOME/profile.log"
profile_only() { echo PROFILE-ONLY; }
PS1='[$?]\$ '
user_hook() { echo "hook:$?" >> "$HOME/hook.log"; }
PROMPT_COMMAND="user_hook${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
trap 'echo "debug:$BASH_COMMAND" >> "$HOME/debug.log"' DEBUG
"#,
    );
    home.write(".bashrc", "echo bashrc >> \"$HOME/profile.log\"\n");
    std::fs::create_dir(home.path.join("dir é")).unwrap();
}

fn assert_clean(session: &Session) {
    let all = session.all();
    for error in [
        "syntax error",
        "not found",
        "conditional binary operator",
        "bad substitution",
        "No such file",
    ] {
        assert!(!all.contains(error), "shell reported {error:?}: {all:?}");
    }
}

/// A command's marks: start (C), its status (D), then the next prompt (A, B) with no
/// command start inside it.
fn assert_command(session: &mut Session, command: &str, status: u8, prompt: &str) -> String {
    session.send(&format!("{command}\r"));
    session.until("\x1b]133;C\x07");
    let output = session.until(&format!("\x1b]133;D;{status}\x07"));
    session.until("\x1b]133;A\x07");
    let prompting = session.until(&format!("{prompt}\x1b]133;B\x07"));
    assert!(
        !prompting.contains("133;C"),
        "a prompt was marked as a command: {prompting:?}"
    );
    output
}

/// An empty line runs nothing: the next prompt carries no command marks.
fn assert_empty_line(session: &mut Session, prompt: &str) {
    session.send("\r");
    let skipped = session.until(&format!("{prompt}\x1b]133;B\x07"));
    assert!(
        !skipped.contains("133;C") && !skipped.contains("133;D"),
        "an empty line was marked as a command: {skipped:?}"
    );
}

#[test]
fn bash_3_2_login_shell_reads_its_profile_once_and_marks_prompts() {
    let Some(home) = isolated(
        module_path!(),
        "bash_3_2_login_shell_reads_its_profile_once_and_marks_prompts",
        &[("SHELL", "/bin/bash")],
    ) else {
        return;
    };
    bash_profile(&home);
    let mut session = Session::login(&home);
    let host = osc7_host();
    session.until(&format!("\x1b]7;file://{host}{}\x07", home.path.display()));
    session.until("\x1b]133;A\x07");
    session.until("[0]$ \x1b]133;B\x07");

    let output = assert_command(
        &mut session,
        r#"shopt login_shell; echo "zero=$0 bash=${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]} ps0=${PS0-unset} ctype=${LC_CTYPE-unset}"; profile_only"#,
        0,
        "[0]$ ",
    );
    assert!(output.contains("login_shell    \ton"), "{output:?}");
    assert!(
        output.contains("zero=-bash bash=3.2 ps0=unset ctype=en_US.UTF-8"),
        "{output:?}"
    );
    assert!(output.contains("PROFILE-ONLY"), "{output:?}");

    assert_command(&mut session, "false", 1, "[1]$ ");
    assert_empty_line(&mut session, "[1]$ ");
    session.send("cd 'dir é'\r");
    session.until(&format!(
        "\x1b]7;file://{host}{}/dir%20%C3%A9\x07",
        home.path.display()
    ));
    session.until("[0]$ \x1b]133;B\x07");
    assert_clean(&session);

    assert_eq!(home.read("profile.log"), "profile\n", "profile read count");
    assert!(
        home.read("hook.log").contains("hook:1\n"),
        "user hook lost $?"
    );
    assert!(
        home.read("debug.log").contains("debug:false\n"),
        "user DEBUG trap lost"
    );
}

#[test]
fn zsh_login_shell_keeps_its_zdotdir_and_marks_prompts() {
    let Some(home) = isolated(
        module_path!(),
        "zsh_login_shell_keeps_its_zdotdir_and_marks_prompts",
        &[("SHELL", "/bin/zsh"), ("ZDOTDIR", "{home}/zdot")],
    ) else {
        return;
    };
    let zdotdir = home.path.join("zdot");
    home.write("zdot/.zshenv", "echo zshenv >> \"$HOME/zshenv.log\"\n");
    home.write(
        "zdot/.zprofile",
        "echo profile >> \"$HOME/profile.log\"\nprofile_only() { echo PROFILE-ONLY; }\n",
    );
    home.write(
        "zdot/.zshrc",
        "PS1='[%?]%# '\nuser_hook() { echo \"hook:$?\" >> \"$HOME/hook.log\"; }\nprecmd_functions+=(user_hook)\n",
    );
    let mut session = Session::login(&home);
    let host = osc7_host();
    session.until(&format!("\x1b]7;file://{host}{}\x07", home.path.display()));
    session.until("[0]% \x1b]133;B\x07");

    let output = assert_command(
        &mut session,
        r#"[[ -o login ]] && echo "login zero=$0 zdotdir=$ZDOTDIR ctype=$LC_CTYPE"; profile_only"#,
        0,
        "[0]% ",
    );
    let expected = format!(
        "login zero=-zsh zdotdir={} ctype=en_US.UTF-8",
        zdotdir.display()
    );
    assert!(output.contains(&expected), "{output:?}");
    assert!(output.contains("PROFILE-ONLY"), "{output:?}");
    assert_command(&mut session, "false", 1, "[1]% ");
    assert_empty_line(&mut session, "[1]% ");
    assert_clean(&session);

    assert_eq!(home.read("profile.log"), "profile\n", "profile read count");
    assert_eq!(home.read("zshenv.log"), "zshenv\n", ".zshenv read count");
    assert!(
        home.read("hook.log").contains("hook:1\n"),
        "user hook lost $?"
    );
}

#[test]
fn a_relative_shell_falls_back_to_the_account_and_keeps_the_locale() {
    let Some(home) = isolated(
        module_path!(),
        "a_relative_shell_falls_back_to_the_account_and_keeps_the_locale",
        &[("SHELL", "bash"), ("LANG", "fr_FR.UTF-8")],
    ) else {
        return;
    };
    let account = account_shell().unwrap().unwrap();
    let name = account.file_name().unwrap().to_str().unwrap();
    home.write(".profile", "");
    home.write(".zshrc", "");
    let mut session = Session::login(&home);
    session.until("\x1b]133;B\x07");
    session.send("echo \"shell=$SHELL zero=$0 lang=$LANG ctype=${LC_CTYPE-unset}\"\r");
    let expected = format!(
        "shell={} zero=-{name} lang=fr_FR.UTF-8 ctype=unset",
        account.display()
    );
    session.until(&expected);
}

#[test]
fn a_login_shell_inherits_no_parent_descriptor() {
    // Apple's bash 3.2 closes inherited descriptors as a login shell; zsh keeps them.
    let Some(home) = isolated(
        module_path!(),
        "a_login_shell_inherits_no_parent_descriptor",
        &[("SHELL", "/bin/zsh")],
    ) else {
        return;
    };
    home.write(".zshrc", "PS1='$ '\n");
    // The bridge, a pane and the config child, with their duplicates, exist before the shell.
    let bridge = sumie_macos::readiness::Bridge::spawn(|| ()).unwrap();
    let mut watches = super::super::sources::Watches::new(bridge.readiness().clone());
    let mut config = std::process::Command::new("/bin/sh");
    config.args(["-c", "exec cat"]);
    let mut reload = super::super::reload::Reload::spawn(config, &mut watches).unwrap();
    let pane = Session::login(&home);
    let pty = &pane.pty;
    watches
        .register_pane(1, pty.output_fd(), pty.exit_fd().unwrap())
        .unwrap();

    let mut session = Session::login(&home);
    session.until("$ \x1b]133;B\x07");
    session.send("/usr/sbin/lsof -nP -F ftdDi -p \"$$\" > child.lsof; echo LSOF-DONE\r");
    session.until("LSOF-DONE\r\n");
    let parent = Descriptors::of(std::process::id());
    let child = Descriptors::parse(&home.read("child.lsof"));
    assert!(
        child.identities.len() >= 3 && parent.kqueues.len() >= 4,
        "lsof listed child {:?}, parent {:?} kqueues",
        child.identities,
        parent.kqueues
    );
    let inherited: Vec<_> = child
        .identities
        .iter()
        .filter(|&(_, identity)| parent.identities.values().any(|own| own == identity))
        .collect();
    assert!(
        inherited.is_empty(),
        "the shell inherited (its fd, identity): {inherited:?} of {:?}",
        parent.identities
    );
    assert!(
        child.kqueues.is_empty(),
        "the shell holds kqueues {:?}",
        child.kqueues
    );
    reload.close(&mut watches).unwrap();
}
