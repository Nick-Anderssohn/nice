//
//  ReleaseFetcher.swift
//  Nice
//
//  GitHub Releases lookup for `ReleaseChecker`. Hidden behind a
//  protocol so unit tests inject a fake and never touch the network.
//
//  `/releases/latest` excludes drafts and pre-releases, so a prerelease
//  of 0.2.0 won't nag users running the stable 0.1.5. GitHub requires
//  a User-Agent on unauthenticated requests; we send one.
//

import Foundation

protocol ReleaseFetcher: Sendable {
    /// Returns the release's `tag_name` exactly as GitHub stores it
    /// (typically `v0.1.5`). Caller is responsible for parsing into
    /// `SemanticVersion`. Throws on any non-2xx or decode failure —
    /// callers treat every throw as "no update info available".
    func fetchLatestTag() async throws -> String
}

struct GitHubReleaseFetcher: ReleaseFetcher {
    static let defaultURL = URL(
        string: "https://api.github.com/repos/Nick-Anderssohn/nice/releases/latest"
    )!

    let url: URL
    let session: URLSession
    let userAgent: String

    init(
        url: URL = Self.defaultURL,
        session: URLSession = .shared,
        userAgent: String = Self.makeDefaultUserAgent()
    ) {
        self.url = url
        self.session = session
        self.userAgent = userAgent
    }

    func fetchLatestTag() async throws -> String {
        var request = URLRequest(url: url)
        request.httpMethod = "GET"
        request.setValue("application/vnd.github+json", forHTTPHeaderField: "Accept")
        request.setValue(userAgent, forHTTPHeaderField: "User-Agent")
        request.timeoutInterval = 10

        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw FetchError.invalidResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            throw FetchError.httpStatus(http.statusCode)
        }
        let decoded = try JSONDecoder().decode(LatestRelease.self, from: data)
        return decoded.tag_name
    }

    enum FetchError: Error {
        case invalidResponse
        case httpStatus(Int)
    }

    private struct LatestRelease: Decodable {
        let tag_name: String
    }

    private static func makeDefaultUserAgent() -> String {
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev"
        return "Nice/\(version) (github.com/Nick-Anderssohn/nice)"
    }
}
