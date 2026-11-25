# Adapted from Ghostty's Kitty-derived bash integration (GPL-3.0-or-later).
# Keep this Bash 3.2 compatible (macOS /bin/bash): no `[[ -v`, and PS0 only from 4.4.
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
    # and runs this entry of PROMPT_COMMAND. Replace the entry with a no-op.
    _sumie_boot='. "$SUMIE_BASH_SCRIPT" && eval "$_sumie_start"'
    export -n PROMPT_COMMAND
    if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
        for _sumie_i in "${!PROMPT_COMMAND[@]}"; do
            PROMPT_COMMAND[_sumie_i]=${PROMPT_COMMAND[_sumie_i]//"$_sumie_boot"/:}
        done
        unset _sumie_i
    else
        # Commands after the entry run before the first prompt, too.
        [[ $PROMPT_COMMAND == *"$_sumie_boot" ]] || _sumie_boot=late
        PROMPT_COMMAND=${PROMPT_COMMAND//"$_sumie_boot"/:}
    fi
fi
source "${SUMIE_BASH_SCRIPT%/bash/*}/cwd.sh"
unset SUMIE_ORIG_ENV SUMIE_BASH_SCRIPT

function ssh { TERM=xterm-256color command ssh -o 'SendEnv=COLORTERM TERM_PROGRAM' "$@"; }

_sumie_ran=0
_sumie_armed=0
_sumie_code=0
_sumie_empty=''
_sumie_status() {
    _sumie_code=$?
    _sumie_armed=0
    if (( _sumie_ran )); then printf '\033]133;D;%d\a' "$_sumie_code"; fi
    _sumie_ran=0
    return "$_sumie_code"
}
_sumie_prompt() {
    _sumie_cwd "$HOSTNAME"
    printf '\033]133;A\a'
    local end='\[\e]133;B\a\]'
    [[ $PS1 == *"$end" ]] || PS1+=$end
    _sumie_armed=1
    # PS1 expands next; keep the command's status for it.
    return "$_sumie_code"
}
if (( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) )); then
    # PS0 expands only for a command, not an empty line or a cancelled edit.
    PS0+='\e]133;C\a${_sumie_empty:0:$((_sumie_ran=1))}'
    _sumie_trap=
else
    # Without PS0, the first DEBUG trap after the prompt marks the command. The user's own
    # DEBUG trap still runs after it, with the same $?, and its status decides the command.
    _sumie_take() { _sumie_user_debug=$1; }
    _sumie_set_status() { return "$1"; }
    _sumie_debug() {
        local code=$?
        if (( _sumie_armed && BASH_SUBSHELL == 0 )) &&
            [[ -z ${COMP_LINE-} && -z ${_sumie_boot-} && $BASH_COMMAND != _sumie_status ]]; then
            _sumie_armed=0
            _sumie_ran=1
            printf '\033]133;C\a'
        fi
        _sumie_set_status "$code"
        eval "$_sumie_user_debug"
    }
    # Bash 3.2 hides the DEBUG trap inside a function or `.` and restores the caller's on
    # return, so this runs through eval at the top level.
    _sumie_trap='_sumie_user_debug=$(trap -p DEBUG)
        [[ -z $_sumie_user_debug ]] || eval "_sumie_take ${_sumie_user_debug#trap -- }"
        trap _sumie_debug DEBUG'
fi
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    PROMPT_COMMAND=(_sumie_status "${PROMPT_COMMAND[@]}" _sumie_prompt)
else
    PROMPT_COMMAND=$'_sumie_status\n'"${PROMPT_COMMAND-}"$'\n_sumie_prompt'
fi
if [[ -z $_sumie_boot ]]; then
    eval "$_sumie_trap"
    unset _sumie_trap _sumie_boot
else
    # The bootstrap entry evals this next: the trap, then the first prompt's marks; the
    # hooks above run from the second prompt. The trap skips commands until _sumie_boot is
    # unset, last. A later entry in this PROMPT_COMMAND would look like the first command.
    _sumie_start=$_sumie_trap'
        _sumie_prompt
        [[ $_sumie_boot != late ]] || _sumie_armed=0
        unset _sumie_trap _sumie_start _sumie_boot'
fi
