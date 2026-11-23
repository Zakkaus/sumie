# Adapted from Ghostty's Kitty-derived bash integration (GPL-3.0-or-later).
# Bash reads ENV instead of its login profiles in POSIX mode.
unset ENV
if [[ -v SUMIE_ORIG_ENV ]]; then export ENV=$SUMIE_ORIG_ENV; fi
source "${SUMIE_BASH_SCRIPT%/bash/*}/cwd.sh"
unset SUMIE_ORIG_ENV SUMIE_BASH_SCRIPT
set +o posix
shopt -u inherit_errexit 2>/dev/null
if [[ $HISTFILE == ~/.sh_history ]]; then HISTFILE=~/.bash_history; fi
[[ ! -r /etc/profile ]] || source /etc/profile
for _sumie_profile in ~/.bash_profile ~/.bash_login ~/.profile; do
    if [[ -r $_sumie_profile ]]; then source "$_sumie_profile"; break; fi
done
unset _sumie_profile

function ssh { TERM=xterm-256color command ssh -o 'SendEnv=COLORTERM TERM_PROGRAM' "$@"; }

_sumie_ran=0
_sumie_empty=''
_sumie_status() {
    local code=$?
    if (( _sumie_ran )); then printf '\033]133;D;%d\a' "$code"; fi
    _sumie_ran=0
    return "$code"
}
_sumie_prompt() {
    _sumie_cwd "$HOSTNAME"
    printf '\033]133;A\a'
    local end='\[\e]133;B\a\]'
    [[ $PS1 == *"$end" ]] || PS1+=$end
}
# PS0 expands only for a command, not an empty line or a cancelled edit.
PS0+='\e]133;C\a${_sumie_empty:0:$((_sumie_ran=1))}'
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    PROMPT_COMMAND=(_sumie_status "${PROMPT_COMMAND[@]}" _sumie_prompt)
else
    PROMPT_COMMAND=$'_sumie_status\n'"${PROMPT_COMMAND-}"$'\n_sumie_prompt'
fi
