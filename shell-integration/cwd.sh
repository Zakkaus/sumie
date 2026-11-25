# Shared by the bash and zsh hooks adapted from Ghostty shell integration.
# Encode bytes rather than characters; OSC terminators in a pathname are data.
_sumie_cwd() {
    local LC_ALL=C rest=$PWD host=$1 uri= byte tail code
    [[ ${_sumie_last_pwd-} != "$PWD" ]] || return 0
    _sumie_last_pwd=$PWD
    while [[ -n $rest ]]; do
        tail=${rest#?}
        byte=${rest%"$tail"}
        case $byte in
            [a-zA-Z0-9/._~-]) uri+=$byte ;;
            # Bash 3.2 reads bytes above 0x7F as negative character codes.
            *) printf -v code '%d' "'$byte"; printf -v byte '%%%02X' $((code & 255)); uri+=$byte ;;
        esac
        rest=$tail
    done
    printf '\033]7;file://%s%s\a' "${host//[^a-zA-Z0-9.-]/}" "$uri"
}
