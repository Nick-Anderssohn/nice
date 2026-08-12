# Nice: discover and stash the user's intended ZDOTDIR, then
# restore our temp dir so zsh keeps reading our other stubs
# (.zprofile / .zshrc). See file header for the cooperation contract.
NICE_TEMP_ZDOTDIR="$ZDOTDIR"
if [[ -n "$NICE_USER_ZDOTDIR" ]]; then
    # Inherited from Nice's launch env (launchctl / parent process).
    USER_ZDOTDIR="$NICE_USER_ZDOTDIR"
else
    # Source ~/.zshenv ourselves to discover any ZDOTDIR set there
    # (XDG-style). This is the FIRST source of ~/.zshenv this
    # session — zsh read OUR stub, not the user's, because ZDOTDIR
    # was overridden — so no double-source / non-idempotency risk.
    unset ZDOTDIR
    [[ -f "$HOME/.zshenv" ]] && source "$HOME/.zshenv"
    USER_ZDOTDIR="${ZDOTDIR:-$HOME}"
fi
export ZDOTDIR="$NICE_TEMP_ZDOTDIR"
export NICE_USER_ZDOTDIR="$USER_ZDOTDIR"
unset NICE_TEMP_ZDOTDIR USER_ZDOTDIR