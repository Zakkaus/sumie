# Adapted from Ghostty's fish integration (MIT, Ghostty contributors).
# Copyright (c) 2024 Mitchell Hashimoto, Ghostty contributors
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
# The above copyright notice and this permission notice shall be included in
# all copies or substantial portions of the Software.
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.

if set -q SUMIE_ORIG_DATA_DIRS
    set -gx XDG_DATA_DIRS "$SUMIE_ORIG_DATA_DIRS"
    set -e SUMIE_ORIG_DATA_DIRS
else
    set -eg XDG_DATA_DIRS
end
status is-interactive; or return

function ssh --wraps ssh
    TERM=xterm-256color command ssh -o 'SendEnv=COLORTERM TERM_PROGRAM' $argv
end

function _sumie_cwd --on-variable PWD
    status is-command-substitution; and return
    printf '\e]7;file://%s%s\a' $hostname (string escape --style=url -- "$PWD")
end
_sumie_cwd

# fish 4 emits A/C/D natively; 4.3 also emits B. Do not duplicate these events.
set -l v (string split . -- $version)
set -l native_acd 0
if test "$v[1]" -ge 4
    status test-feature mark-prompt
    if test $status -ne 1 # early 4.0 releases always mark; the feature flag was added later
        if test "$v[1]" -gt 4; or test "$v[2]" -ge 3
            return
        end
        set native_acd 1
    end
end

# Capture the user's prompt after config.fish has defined it.
function _sumie_setup --on-event fish_prompt
    functions -e _sumie_setup
    functions -c fish_prompt _sumie_user_prompt
    function fish_prompt
        _sumie_user_prompt
        printf '\e]133;B\a'
    end
end
if test $native_acd -eq 1
    return
end
function _sumie_mark_prompt --on-event fish_prompt
    printf '\e]133;A\a'
end
function _sumie_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
function _sumie_postexec --on-event fish_postexec
    printf '\e]133;D;%d\a' $status
end
