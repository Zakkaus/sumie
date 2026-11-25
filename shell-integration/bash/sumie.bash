# Adapted from Ghostty's Kitty-derived bash integration (GPL-3.0-or-later).
# Keep this Bash 3.2 compatible (macOS /bin/bash): no `[[ -v`, and PS0 only from 4.4.
# Before 4.4, a DEBUG trap installed after this one (bash-preexec's, at its first prompt)
# is chained from the next prompt; the command in between goes unmarked.
if [[ ${ENV-} == '${SUMIE_BASH_SCRIPT}' ]]; then
    # Linux starts bash in POSIX mode, which reads ENV instead of the login profiles.
    unset ENV
    if [[ -n ${SUMIE_ORIG_ENV+set} ]]; then export ENV=$SUMIE_ORIG_ENV; fi
    set +o posix
    shopt -u inherit_errexit 2>/dev/null
    if [[ $HISTFILE == ~/.sh_history ]]; then HISTFILE=~/.bash_history; fi
    [[ ! -r /etc/profile ]] || source /etc/profile
    for _sumie_profile in ~/.bash_profile ~/.bash_login ~/.profile; do
        if [[ -r $_sumie_profile ]]; then source "$_sumie_profile"; break; fi
    done
    unset _sumie_profile
    _sumie_boot=
else
    # macOS: bash 3.2 reads ENV only as sh, so a plain login shell has read its profiles
    # and runs the bootstrap entry of PROMPT_COMMAND. Replace that exact text wherever the
    # profiles left it with a no-op that keeps $? for what follows (`; user_hook`); commands
    # after it still run before the first prompt.
    _sumie_boot='[[ -n ${SUMIE_BASH_SCRIPT-} ]] && . "$SUMIE_BASH_SCRIPT" && eval "$_sumie_start"'
    _sumie_keep() { return "$?"; }
    export -n PROMPT_COMMAND
    if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
        for _sumie_i in "${!PROMPT_COMMAND[@]}"; do
            PROMPT_COMMAND[_sumie_i]=${PROMPT_COMMAND[_sumie_i]//"$_sumie_boot"/_sumie_keep}
        done
        unset _sumie_i
    else
        # An element assignment would make the string an array, which bash < 5.1 ignores.
        PROMPT_COMMAND=${PROMPT_COMMAND//"$_sumie_boot"/'${_sumie_ran+_sumie_keep}'}
    fi
fi
source "${SUMIE_BASH_SCRIPT%/bash/*}/cwd.sh"
unset SUMIE_ORIG_ENV SUMIE_BASH_SCRIPT

function ssh { TERM=xterm-256color command ssh -o 'SendEnv=COLORTERM TERM_PROGRAM' "$@"; }

_sumie_ran=0
_sumie_armed=
_sumie_code=0
_sumie_empty=''
_sumie_status() {
    _sumie_code=$?
    _sumie_armed=
    if (( _sumie_ran )); then printf '\033]133;D;%d\a' "$_sumie_code"; fi
    _sumie_ran=0
    return "$_sumie_code"
}
_sumie_prompt() {
    _sumie_cwd "$HOSTNAME"
    printf '\033]133;A\a'
    local end=$_sumie_end
    if ! shopt -q promptvars; then
        end='\[\e]133;B\a\]'
        _sumie_armed=1
    fi
    [[ $PS1 == *"$end" ]] || PS1+=$end
    # PS1 expands next; keep the command's status for it.
    return "$_sumie_code"
}
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    # PS0 expands only for a command, not an empty line or a cancelled edit.
    PS0+='\e]133;C\a${_sumie_empty:0:$((_sumie_ran=1))}'
    _sumie_end='\[\e]133;B\a\]'
    _sumie_rearm=
else
    # Without PS0, the first DEBUG trap after the prompt marks the command. The user's own
    # DEBUG trap still runs after it, with the same $?, and its status decides the command.
    # PS1 arms the mark as it expands, after every PROMPT_COMMAND entry has run; it expands
    # nothing (3.2 skips nested expansions of an empty value, hence _sumie_one).
    _sumie_one=1
    _sumie_end='\[\e]133;B\a\]${_sumie_one#${_sumie_armed:=1}}'
    _sumie_user_debug=
    _sumie_hooked=0
    _sumie_take() { [[ $1 == _sumie_debug ]] || _sumie_user_debug=$1; }
    _sumie_set_status() { return "$1"; }
    _sumie_debug() {
        local code=$?
        _sumie_hooked=1
        # A chained trap can call this again (bash-preexec runs the trap it replaced).
        if [[ -n ${_sumie_chaining-} ]]; then return "$code"; fi
        if [[ -n $_sumie_armed && -z ${COMP_LINE-} ]] && (( BASH_SUBSHELL == 0 )) &&
            [[ $BASH_COMMAND != '${_sumie_ran+_sumie_status}' ]]; then
            _sumie_armed=
            _sumie_ran=1
            printf '\033]133;C\a'
        fi
        _sumie_chaining=1
        _sumie_set_status "$code"
        eval "$_sumie_user_debug"
        code=$?
        _sumie_chaining=
        return "$code"
    }
    # Bash 3.2 hides the DEBUG trap inside a function or `.` and restores the caller's on
    # return, so this runs through eval at the top level.
    _sumie_trap='_sumie_found=$(trap -p DEBUG)
        eval "_sumie_take ${_sumie_found#trap -- }"
        unset _sumie_found
        trap _sumie_debug DEBUG'
    # A later DEBUG trap (bash-preexec's, installed at its first prompt, or one set at the
    # prompt) replaces sumie's, so each prompt reinstalls it in front of the new one. The
    # trap runs before the test, and sets _sumie_hooked, only while it is installed.
    _sumie_rearm=$'_sumie_hooked=0; [[ $_sumie_hooked == 1 ]] || eval "${_sumie_trap-}"\n'
fi
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    PROMPT_COMMAND=(_sumie_status "${PROMPT_COMMAND[@]}" _sumie_prompt)
else
    # Bash exports no array, but a string can be re-exported: keep it inert in a nested
    # bash, which has none of these functions.
    PROMPT_COMMAND='${_sumie_ran+_sumie_status}'$'\n'"${PROMPT_COMMAND-}"$'\n'"$_sumie_rearm"'${_sumie_ran+_sumie_prompt}'
fi
unset _sumie_rearm
if [[ -z ${_sumie_boot-} ]]; then
    eval "${_sumie_trap-}"
else
    # The bootstrap entry evals this next: the trap, then the first prompt's marks. The
    # hooks above run from the second prompt.
    _sumie_start=${_sumie_trap-}'
        _sumie_prompt
        unset _sumie_start'
fi
unset _sumie_boot
