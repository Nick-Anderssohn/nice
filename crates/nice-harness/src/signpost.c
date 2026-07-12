// nice-harness — os_signpost "Frame" interval shim for the Nice RS self-test
// harness. Ported from the phase-0 headline shim (nice_signpost.c), renamed to
// the rewrite's subsystem.
//
// Why C and not Rust FFI: the os_signpost macros place the name/format strings
// in the special __TEXT sections the signpost runtime expects and pass the
// correct dso handle, so intervals are guaranteed visible to
// `xctrace record --template Logging`. Hand-rolled Rust calls into
// `_os_signpost_emit_with_name_impl` can silently emit nothing if either detail
// is off.
//
// Contract (keep in sync with crates/README.md when the docs slice lands):
//   subsystem: "dev.nickanderssohn.nice"
//   category : "selftest"
//   name     : "Frame"
//
// Semantics: one interval per self-test repaint (the animated view brackets its
// render body). CPU-side only, not GPU-complete. Zero overhead when no recorder
// is attached: os_signpost_enabled() gates begin; a null id makes end a no-op.

#include <dispatch/dispatch.h>
#include <os/log.h>
#include <os/signpost.h>
#include <stdint.h>

static os_log_t nice_rs_log(void) {
    static os_log_t log;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        log = os_log_create("dev.nickanderssohn.nice", "selftest");
    });
    return log;
}

uint64_t nice_rs_signpost_frame_begin(void) {
    os_log_t log = nice_rs_log();
    if (!os_signpost_enabled(log)) {
        return (uint64_t)OS_SIGNPOST_ID_NULL;
    }
    os_signpost_id_t spid = os_signpost_id_generate(log);
    if (spid == OS_SIGNPOST_ID_NULL || spid == OS_SIGNPOST_ID_INVALID) {
        return (uint64_t)OS_SIGNPOST_ID_NULL;
    }
    os_signpost_interval_begin(log, spid, "Frame");
    return (uint64_t)spid;
}

void nice_rs_signpost_frame_end(uint64_t spid) {
    if (spid == (uint64_t)OS_SIGNPOST_ID_NULL ||
        spid == (uint64_t)OS_SIGNPOST_ID_INVALID) {
        return;
    }
    os_signpost_interval_end(nice_rs_log(), (os_signpost_id_t)spid, "Frame");
}
