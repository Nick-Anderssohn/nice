// NICE Phase-0 §13 spike 11 — os_signpost "Draw" interval shim for the
// HEADLINE harness on the zed-main pin (port of the 0.2.2 vendored-gpui shim,
// spikes/phase0-poc/vendor/gpui-0.2.2/src/platform/mac/nice_signpost.c).
//
// Why C and not Rust FFI: the os_signpost macros place the name/format
// strings in the special __TEXT sections the signpost runtime expects and
// pass the correct dso handle, so intervals are guaranteed visible to
// `xctrace record --template Logging`. Hand-rolled Rust calls into
// `_os_signpost_emit_with_name_impl` can silently emit nothing if either
// detail is off.
//
// Contract (keep in sync with the headline runbook/summary):
//   subsystem: "dev.nickanderssohn.gpui-term-main"
//   category : "present"
//   name     : "Draw"
//
// Semantics: one interval per MetalRenderer::draw on the PATCHED zed-main
// checkout (../zed-main-patched + zed-main-headline-hook.patch) — CPU-side
// scene submission (drawable acquire -> encode -> commandBuffer commit
// [-> waitUntilScheduled -> drawable.present() in transactional mode]).
// NOT GPU-complete. Symmetric to the 0.2.2 spike's "Draw" (subsystem
// dev.nickanderssohn.gpui-term) and to SwiftTerm's "Metal.Draw" (subsystem
// org.tirania.SwiftTerm, category MetalProfile).
//
// Unlike the 0.2.2 shim, the draw COUNT lives Rust-side in the patched
// gpui_macos (nice_draw_metrics::DRAW_COUNT) so it exists even in binaries
// that never register these hooks; this shim only emits the signposts.
//
// Zero overhead when no recorder is attached: os_signpost_enabled() gates
// begin; a 0 id makes end a no-op.

#include <dispatch/dispatch.h>
#include <os/log.h>
#include <os/signpost.h>
#include <stdint.h>

static os_log_t nice_main_log(void) {
    static os_log_t log;
    static dispatch_once_t once;
    dispatch_once(&once, ^{
        log = os_log_create("dev.nickanderssohn.gpui-term-main", "present");
    });
    return log;
}

uint64_t nice_signpost_draw_begin(void) {
    os_log_t log = nice_main_log();
    if (!os_signpost_enabled(log)) {
        return (uint64_t)OS_SIGNPOST_ID_NULL;
    }
    os_signpost_id_t spid = os_signpost_id_generate(log);
    if (spid == OS_SIGNPOST_ID_NULL || spid == OS_SIGNPOST_ID_INVALID) {
        return (uint64_t)OS_SIGNPOST_ID_NULL;
    }
    os_signpost_interval_begin(log, spid, "Draw");
    return (uint64_t)spid;
}

void nice_signpost_draw_end(uint64_t spid) {
    if (spid == (uint64_t)OS_SIGNPOST_ID_NULL ||
        spid == (uint64_t)OS_SIGNPOST_ID_INVALID) {
        return;
    }
    os_signpost_interval_end(nice_main_log(), (os_signpost_id_t)spid, "Draw");
}
