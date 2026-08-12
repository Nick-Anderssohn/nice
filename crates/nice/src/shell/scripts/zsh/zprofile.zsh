# Nice: source the user's real .zprofile from the location resolved
# in our .zshenv. (Without this, login-shell users silently lose
# .zprofile because zsh's $ZDOTDIR/.zprofile lookup hits our stub.)
[[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zprofile" ]] \
    && source "$NICE_USER_ZDOTDIR/.zprofile"