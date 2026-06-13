//
//  NiceServicesTearOffTokenTests.swift
//  NiceUnitTests
//
//  Phase B coverage for the token-keyed tear-off seed pairing that
//  replaces the old temporal FIFO. A seed is deposited under an explicit
//  UUID token and consumed only by the window opened for THAT token —
//  killing the "seed-steal" class where a ⌘N / restore window that
//  mounted first could pop a tear-off's seed.
//
//  Covers:
//    • Deposit-then-consume by token returns the seed and is one-shot.
//    • Consuming by the WRONG token returns nil and does NOT consume the
//      real token's seed (the FIFO-ordering independence this phase
//      buys — there is no "next seed", only "this token's seed").
//    • Two seeds under distinct tokens are independently retrievable in
//      any order (no FIFO dependence).
//    • Eviction: depositing more than the cap (8) drops the OLDEST token
//      (bounds the orphan-seed leak when a deposited window never opens),
//      while recent tokens remain retrievable.
//

import AppKit
import XCTest
@testable import Nice

@MainActor
final class NiceServicesTearOffTokenTests: XCTestCase {

    private var services: NiceServices!

    override func setUp() {
        super.setUp()
        services = NiceServices()
    }

    override func tearDown() {
        services = nil
        super.tearDown()
    }

    // MARK: - Helpers

    /// A `PendingTearOff` with a nil entry (the entry is optional
    /// post-Phase-A — a deferred pane spawns fresh in the destination
    /// from `cwd`) and a recognizable `paneId` so tests can assert which
    /// seed came back.
    private func makeSeed(paneId: String, cwd: String = "/tmp") -> NiceServices.PendingTearOff {
        NiceServices.PendingTearOff(
            entry: nil,
            paneId: paneId,
            title: "Pane \(paneId)",
            kind: .terminal,
            claudeSessionId: nil,
            projectId: "p-\(paneId)",
            projectName: paneId.uppercased(),
            projectPath: cwd,
            cwd: cwd,
            screenPoint: NSPoint(x: 10, y: 20)
        )
    }

    // MARK: - Deposit / consume by token

    func test_depositThenConsumeByToken_returnsSeed_andIsOneShot() {
        services.enqueueTearOff(makeSeed(paneId: "pA"), token: "t1")

        // First consume returns the seed paired to "t1".
        guard let seed = services.consumeTearOffSeed(token: "t1") else {
            return XCTFail("Expected the seed deposited under t1")
        }
        XCTAssertEqual(seed.paneId, "pA")

        // One-shot: a second consume of the same token is nil.
        XCTAssertNil(services.consumeTearOffSeed(token: "t1"),
                     "Seed must be consumed exactly once")
    }

    func test_consumeWrongToken_returnsNil_andDoesNotConsumeRealToken() {
        services.enqueueTearOff(makeSeed(paneId: "pA"), token: "t1")

        // A window with a different (wrong) token — e.g. a fan-out window
        // whose token has no deposited seed — gets nil.
        XCTAssertNil(services.consumeTearOffSeed(token: "wrong"),
                     "A token with no deposited seed must return nil")

        // The real seed is untouched and still retrievable by its token
        // (no FIFO "consume the next one" semantics).
        guard let seed = services.consumeTearOffSeed(token: "t1") else {
            return XCTFail("t1's seed must survive a wrong-token consume")
        }
        XCTAssertEqual(seed.paneId, "pA")
    }

    func test_twoTokens_independentlyRetrievable_noFifoOrdering() {
        services.enqueueTearOff(makeSeed(paneId: "pA"), token: "t1")
        services.enqueueTearOff(makeSeed(paneId: "pB"), token: "t2")

        // Consume in REVERSE deposit order — a FIFO would have handed back
        // pA first; token pairing hands back exactly the requested seed.
        XCTAssertEqual(services.consumeTearOffSeed(token: "t2")?.paneId, "pB")
        XCTAssertEqual(services.consumeTearOffSeed(token: "t1")?.paneId, "pA")

        // Both consumed; nothing lingers.
        XCTAssertNil(services.consumeTearOffSeed(token: "t1"))
        XCTAssertNil(services.consumeTearOffSeed(token: "t2"))
    }

    // MARK: - Bounded eviction

    func test_depositingPastCap_evictsOldestToken() {
        // Cap is 8 (documented in NiceServices). Deposit 9 seeds: the
        // first (oldest) must be evicted when the 9th lands, while the
        // most recent ones remain. Simulates the orphan-seed leak where a
        // deposited window never opens.
        let cap = 8
        let tokens = (0...cap).map { "t\($0)" }  // t0 ... t8  (9 tokens)
        for token in tokens {
            services.enqueueTearOff(makeSeed(paneId: token), token: token)
        }

        // Oldest (t0) evicted — its seed is no longer retrievable.
        XCTAssertNil(services.consumeTearOffSeed(token: tokens.first!),
                     "Oldest token's seed must be evicted past the cap")

        // The remaining `cap` tokens (t1 ... t8) survive.
        for token in tokens.dropFirst() {
            XCTAssertEqual(services.consumeTearOffSeed(token: token)?.paneId, token,
                           "Recent token \(token) must still be retrievable")
        }
    }
}
