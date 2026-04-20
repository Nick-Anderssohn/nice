# Nice — working rules for Claude

## Never kill a running Nice without asking

Nice hosts the user's live Claude Code sessions in long-lived ptys. Killing `Nice` mid-session loses work and breaks whatever the user is doing right now — including, often, the conversation you are in.

**Before any action that would terminate the `Nice` process, check whether it is running:**

```sh
pgrep -x Nice
```

If it is running, **pause and ask for explicit permission** before proceeding. Do not assume "the user will restart it" is fine.

### Common actions that kill Nice

- `pkill -x Nice`, `killall Nice`, `kill <pid>` targeting Nice
- `scripts/install.sh` and the `/nice-install` command — the install step quits any running Nice (see `scripts/install.sh:92-105`)
- `xcodebuild` runs that relaunch the app, and UITests in `UITests/` that drive `Nice.app`
- `rm`/`mv` against `/Applications/Nice.app` while it is running

If the user has already authorized the action in the current turn (e.g. "reinstall Nice"), you may proceed without re-asking. Authorization does not carry across unrelated tasks.

### If killing is genuinely necessary for debugging

Stop and ask. Offer the alternative of a graceful quit first:

```sh
osascript -e 'tell application "Nice" to quit'
```

Only escalate to `pkill`/SIGKILL with explicit user consent.
