#!/usr/bin/env bash
#
# worktree-lock.sh — serialize operations that can't run concurrently
# across git worktrees of this repo (global install, UI tests, xcodebuild
# against shared DerivedData). The lock lives outside the repo so every
# worktree and the main checkout contend on the same file.
#
# Usage:
#   scripts/worktree-lock.sh acquire <op-name>   # block until we hold the lock
#   scripts/worktree-lock.sh release              # release (safe — only if we hold it)
#   scripts/worktree-lock.sh status               # print current holder, if any
#   scripts/worktree-lock.sh break                # force-release (use with care)
#
# Stale locks (older than the TTL) are auto-broken on the next acquire.

set -euo pipefail

LOCK_ROOT="${HOME}/.claude/locks"
LOCK_DIR="${LOCK_ROOT}/nice.lock"
HOLDER_FILE="${LOCK_DIR}/holder"
TTL_SECONDS="${NICE_LOCK_TTL:-1800}"          # 30 min
POLL_SECONDS="${NICE_LOCK_POLL:-5}"
MAX_WAIT_SECONDS="${NICE_LOCK_MAX_WAIT:-0}"   # 0 = wait forever (bounded by TTL)

my_path() {
    git rev-parse --show-toplevel 2>/dev/null || pwd
}

lock_mtime() {
    stat -f %m "${HOLDER_FILE}" 2>/dev/null || echo 0
}

lock_age() {
    local now
    now=$(date +%s)
    echo $(( now - $(lock_mtime) ))
}

read_holder_field() {
    local key="$1"
    [[ -f "${HOLDER_FILE}" ]] || return 0
    awk -F= -v k="${key}" '$1==k { sub(/^[^=]*=/,""); print; exit }' "${HOLDER_FILE}"
}

break_if_stale() {
    [[ -d "${LOCK_DIR}" ]] || return 0
    local age
    age=$(lock_age)
    if (( age > TTL_SECONDS )); then
        printf '[nice-lock] stale lock (age=%ss > ttl=%ss), breaking:\n' "${age}" "${TTL_SECONDS}" >&2
        [[ -f "${HOLDER_FILE}" ]] && sed 's/^/[nice-lock]   /' "${HOLDER_FILE}" >&2
        rm -rf "${LOCK_DIR}"
    fi
}

write_holder() {
    local op="$1"
    {
        printf 'holder=%s\n' "$(my_path)"
        printf 'operation=%s\n' "${op}"
        printf 'acquired_at=%s\n' "$(date +%s)"
        printf 'pid=%s\n' "$$"
        printf 'host=%s\n' "$(hostname)"
    } > "${HOLDER_FILE}"
}

cmd_acquire() {
    local op="${1:-unnamed}"
    mkdir -p "${LOCK_ROOT}"
    local start_wait
    start_wait=$(date +%s)
    local announced_wait=0
    while true; do
        break_if_stale
        if mkdir "${LOCK_DIR}" 2>/dev/null; then
            write_holder "${op}"
            printf '[nice-lock] acquired for op=%s by %s\n' "${op}" "$(my_path)" >&2
            return 0
        fi
        if (( announced_wait == 0 )); then
            printf '[nice-lock] waiting — lock is held:\n' >&2
            [[ -f "${HOLDER_FILE}" ]] && sed 's/^/[nice-lock]   /' "${HOLDER_FILE}" >&2
            announced_wait=1
        fi
        if (( MAX_WAIT_SECONDS > 0 )); then
            local waited=$(( $(date +%s) - start_wait ))
            if (( waited >= MAX_WAIT_SECONDS )); then
                printf '[nice-lock] gave up waiting after %ss\n' "${waited}" >&2
                return 3
            fi
        fi
        sleep "${POLL_SECONDS}"
    done
}

cmd_release() {
    if [[ ! -d "${LOCK_DIR}" ]]; then
        printf '[nice-lock] no lock to release\n' >&2
        return 0
    fi
    local holder
    holder=$(read_holder_field holder)
    local mine
    mine=$(my_path)
    if [[ -n "${holder}" && "${holder}" != "${mine}" ]]; then
        printf '[nice-lock] refusing to release — held by %s, not %s\n' "${holder}" "${mine}" >&2
        printf '[nice-lock] run "%s break" if you need to force-release\n' "$0" >&2
        return 1
    fi
    rm -rf "${LOCK_DIR}"
    printf '[nice-lock] released\n' >&2
}

cmd_status() {
    if [[ ! -d "${LOCK_DIR}" ]]; then
        echo "no lock held"
        return 0
    fi
    printf 'lock held (age=%ss):\n' "$(lock_age)"
    [[ -f "${HOLDER_FILE}" ]] && sed 's/^/  /' "${HOLDER_FILE}"
}

cmd_break() {
    if [[ ! -d "${LOCK_DIR}" ]]; then
        echo "no lock to break" >&2
        return 0
    fi
    printf '[nice-lock] force-breaking lock; prior holder was:\n' >&2
    [[ -f "${HOLDER_FILE}" ]] && sed 's/^/[nice-lock]   /' "${HOLDER_FILE}" >&2
    rm -rf "${LOCK_DIR}"
    printf '[nice-lock] released\n' >&2
}

case "${1:-}" in
    acquire) shift; cmd_acquire "$@" ;;
    release) cmd_release ;;
    status)  cmd_status ;;
    break)   cmd_break ;;
    *)
        cat <<EOF >&2
Usage: $0 {acquire <op-name> | release | status | break}

Env vars:
  NICE_LOCK_TTL       stale threshold in seconds (default 1800)
  NICE_LOCK_POLL      poll interval in seconds while waiting (default 5)
  NICE_LOCK_MAX_WAIT  give up after N seconds waiting (default 0 = forever)
EOF
        exit 2
        ;;
esac
