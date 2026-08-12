# Nice: defensive — if our .zshrc somehow exited before restoring
# ZDOTDIR (user .zshrc errored out, etc.), source the user's real
# .zlogin from where they actually keep it. In the success path
# ZDOTDIR has already been restored to the user's value by our
# .zshrc, so zsh reads the user's .zlogin directly and this stub
# is never reached.
[[ -n "$NICE_USER_ZDOTDIR" && -f "$NICE_USER_ZDOTDIR/.zlogin" ]] \
    && source "$NICE_USER_ZDOTDIR/.zlogin"