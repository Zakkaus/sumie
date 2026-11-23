# Adapted from Ghostty's Kitty-derived ZDOTDIR shim (GPL-3.0-or-later).
source "$ZDOTDIR/../cwd.sh"
if (( ${+SUMIE_ORIG_ZDOTDIR} )); then
    export ZDOTDIR=$SUMIE_ORIG_ZDOTDIR
else
    unset ZDOTDIR
fi
unset SUMIE_ORIG_ZDOTDIR
[[ ! -r ${ZDOTDIR-$HOME}/.zshenv ]] || source "${ZDOTDIR-$HOME}/.zshenv"
[[ -o interactive ]] || return

function ssh { TERM=xterm-256color command ssh -o 'SendEnv=COLORTERM TERM_PROGRAM' "$@"; }

_sumie_ran=0

_sumie_preexec() {
    _sumie_ran=1
    printf '\033]133;C\a'
}
_sumie_precmd() {
    local code=$?
    if (( _sumie_ran )); then printf '\033]133;D;%d\a' "$code"; fi
    _sumie_ran=0
    _sumie_cwd "$HOST"
    printf '\033]133;A\a'
    local end=$'%{\e]133;B\a%}'
    [[ $PS1 == *"$end" ]] || PS1+=$end
    # Prompt themes loaded by .zshrc may replace PS1 in their own precmd hook.
    if [[ ${precmd_functions[-1]} != _sumie_precmd ]]; then
        precmd_functions=(${precmd_functions:#_sumie_precmd} _sumie_precmd)
    fi
}
autoload -Uz add-zsh-hook
add-zsh-hook precmd _sumie_precmd
add-zsh-hook preexec _sumie_preexec
