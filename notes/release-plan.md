# Publish Nice via Homebrew (personal tap, signed + notarized)

## Context

Nice is in good shape and ready for a first public release. Today it's distributed only via `scripts/install.sh` (ad-hoc signed, requires Xcode + XcodeGen on the machine), which is fine for contributors but a rough first-run experience for users. Goal: let anyone install Nice with one command —

```sh
brew install --cask Nick-Anderssohn/nice/nice
```

— getting a properly signed + notarized build that launches without Gatekeeper friction.

The official `homebrew/homebrew-cask` tap is out of reach initially: self-submissions require ≥225 stars / 90 forks / 90 watchers, and `nice` has none of those yet. A **personal tap** (`Nick-Anderssohn/homebrew-nice`) has no notability gate and gives us the same `brew install --cask` UX. We'll graduate to the official tap later once the repo gains traction.

Homebrew now enforces signing + notarization on casks (hard cutoff Sept 2026), so we do that work once and reuse it regardless of tap.

## Prerequisites (you do these, not Claude)

These happen outside the repo. They block Phase 2 onward.

1. **Enroll in the Apple Developer Program** — $99/year at developer.apple.com. As an individual (faster than Organization, which needs a D-U-N-S number).
2. **Create a Developer ID Application certificate** — In Xcode → Settings → Accounts → Manage Certificates → `+` → Developer ID Application. This is the one for distributing outside the App Store; don't confuse it with the "Apple Development" or "Mac Installer" certificates.
3. **Create an app-specific password for `notarytool`** — At appleid.apple.com → Sign-In & Security → App-Specific Passwords. Label it something like "nice-notarization". You'll use this (not your Apple ID password) for notarization.
4. **Grab your Team ID** — Visible in developer.apple.com → Membership, or in Xcode → Settings → Accounts. 10-char alphanumeric.

Keep these four things handy: **cert in Keychain**, **Apple ID email**, **app-specific password**, **Team ID**.

## Approach

Five pieces, in order. Phases 1–2 are the bulk of the work; 3–5 are small once the artifact pipeline is solid.

### Phase 1 — Local release script (`scripts/release.sh`)

A single script that, given a version, produces a notarized `.zip` ready for upload to GitHub Releases. Ship this first because iterating on signing/notarization is much faster locally than in CI.

**What it does:**

1. Accept `--version X.Y.Z` (or read from git tag when invoked from CI).
2. Update `CFBundleShortVersionString` / `MARKETING_VERSION` / `CFBundleVersion` in `project.yml` to the requested version (simple `sed`-style edit — the fields are literal strings in the YAML at known keys).
3. Run `xcodegen generate`.
4. `xcodebuild archive` with real signing:
   ```
   xcodebuild -project Nice.xcodeproj -scheme Nice -configuration Release \
     -archivePath build/Nice.xcarchive \
     -destination 'generic/platform=macOS' \
     CODE_SIGN_STYLE=Manual \
     CODE_SIGN_IDENTITY="Developer ID Application: <Your Name> (<TEAM_ID>)" \
     DEVELOPMENT_TEAM=<TEAM_ID> \
     archive
   ```
5. Export the archive to a plain `.app` using `xcodebuild -exportArchive` with an `ExportOptions.plist` that specifies `method=developer-id` and `signingStyle=manual`.
6. **Zip** the `.app` using `ditto -c -k --keepParent` (preserves resource forks and Mach-O signing metadata; `zip` does not).
7. **Notarize**:
   ```
   xcrun notarytool submit build/Nice.zip \
     --apple-id "$APPLE_ID" \
     --password "$APPLE_APP_PASSWORD" \
     --team-id "$APPLE_TEAM_ID" \
     --wait
   ```
8. **Staple** the ticket onto the `.app` (Gatekeeper can then verify offline):
   ```
   xcrun stapler staple build/Build/Products/Release/Nice.app
   ```
9. **Re-zip after stapling** (the staple lives inside the bundle — the pre-notarization zip doesn't have it).
10. Print the final zip path + its `shasum -a 256` — that SHA is what the cask will pin.

**Why `.zip` not `.dmg`:** zips are simpler, notarize the same way, and casks handle them fine. A `.dmg` adds a signing/notarization step for the DMG itself and a background image to design. Skip it for now; it's easy to switch later if we want the drag-to-Applications window.

**Entitlements sanity check:** Nice already has `ENABLE_HARDENED_RUNTIME: YES` and no risky entitlements. It spawns child processes via `forkpty` — allowed under hardened runtime without any special entitlement (sandbox is off). Notarization should pass cleanly on the first try; if it rejects, `notarytool log <submission-id>` prints the exact reason. The two common causes that aren't true here would be missing hardened runtime or a JIT/disable-library-validation entitlement without justification.

**Secrets for local runs:** Read `APPLE_ID`, `APPLE_APP_PASSWORD`, `APPLE_TEAM_ID` from env vars (or a gitignored `.env.release` the script sources). Never commit them.

**Files to create/modify:**
- `scripts/release.sh` (new) — the script above.
- `scripts/ExportOptions.plist` (new) — `method=developer-id`, `signingStyle=manual`, `teamID`.
- `scripts/bump-version.sh` or inline in `release.sh` — update `project.yml` version fields.
- `.gitignore` — add `build/`, `.env.release`.

### Phase 2 — GitHub Actions release workflow

Once Phase 1 runs clean locally, wrap it in CI so tagging a release is the only manual step.

**Trigger:** push of tag matching `v*.*.*`.

**Runner:** `macos-14` (or `macos-latest`) — needs Xcode 16 for Swift 6. Pin to a specific image when it starts mattering.

**Steps:**

1. `actions/checkout@v4`.
2. `brew install xcodegen` (not preinstalled on GitHub's mac runners by default; ~10s).
3. **Import signing cert** using `apple-actions/import-codesign-certs@v3` (well-maintained, widely used). Takes a base64-encoded `.p12` export of the cert+private key and a password; writes them to a temporary keychain that's torn down at job end.
4. Run `scripts/release.sh --version "${GITHUB_REF_NAME#v}"`.
5. `softprops/action-gh-release@v2` — upload `Nice-X.Y.Z.zip` to the GitHub Release created from the tag. Also writes release notes from commits.
6. Emit the artifact's SHA256 and the download URL as workflow outputs (next phase consumes them).

**Repo secrets to add** (Settings → Secrets and variables → Actions):
- `APPLE_CERT_P12_BASE64` — `base64 < DeveloperID.p12`. Export the cert from Keychain Access → right-click → Export → `.p12` with a strong password.
- `APPLE_CERT_P12_PASSWORD` — the password you used when exporting.
- `APPLE_ID` — Apple ID email.
- `APPLE_APP_PASSWORD` — app-specific password from prerequisite (3).
- `APPLE_TEAM_ID` — Team ID from prerequisite (4).
- `HOMEBREW_TAP_TOKEN` — used in Phase 4; a fine-grained PAT with contents:write on the tap repo only. Don't use a classic token with broad scopes.

**File to create:** `.github/workflows/release.yml`.

### Phase 3 — Create the tap repo

A Homebrew "tap" is just a GitHub repo named `homebrew-<something>` containing a `Casks/` (or `Formula/`) directory. For us: **`Nick-Anderssohn/homebrew-nice`**, with `Casks/nice.rb`.

**Bootstrap:**

1. Create the public repo `Nick-Anderssohn/homebrew-nice` on GitHub (empty, MIT license for consistency).
2. Add `Casks/nice.rb`:
   ```ruby
   cask "nice" do
     version "0.1.0"
     sha256 "<sha256 of Nice-0.1.0.zip>"

     url "https://github.com/Nick-Anderssohn/nice/releases/download/v#{version}/Nice-#{version}.zip"
     name "Nice"
     desc "Native macOS GUI for Claude Code"
     homepage "https://github.com/Nick-Anderssohn/nice"

     depends_on macos: ">= :sonoma"

     app "Nice.app"

     zap trash: [
       "~/Library/Preferences/dev.nickanderssohn.nice.plist",
       "~/Library/Saved Application State/dev.nickanderssohn.nice.savedState",
     ]
   end
   ```
3. Verify locally once before publishing:
   ```
   brew tap Nick-Anderssohn/nice
   brew install --cask nice
   brew audit --cask --strict nice  # homebrew-cask's own lint
   ```

The `zap` block is what `brew uninstall --zap` uses to wipe settings — matches Nice's actual UserDefaults domain (`dev.nickanderssohn.nice`, confirmed in `project.yml:65`).

### Phase 4 — Automated cask bumps

Every tag push in `nice` should update `Casks/nice.rb` in the tap repo with the new version + SHA256. Otherwise Phase 3 becomes manual forever.

Use `dawidd6/action-homebrew-bump-formula@v4` (supports casks too) or the simpler approach of having `release.yml` check out the tap, `sed` the version + sha, and open a PR with `peter-evans/create-pull-request@v6`. The PR approach is slightly more code but fully transparent and easy to debug — recommend that one.

Add as a final step to `.github/workflows/release.yml`:

```yaml
- uses: actions/checkout@v4
  with:
    repository: Nick-Anderssohn/homebrew-nice
    token: ${{ secrets.HOMEBREW_TAP_TOKEN }}
    path: tap
- run: |
    cd tap
    sed -i '' -E "s/version \".*\"/version \"${VERSION}\"/" Casks/nice.rb
    sed -i '' -E "s/sha256 \".*\"/sha256 \"${SHA256}\"/" Casks/nice.rb
- uses: peter-evans/create-pull-request@v6
  with:
    path: tap
    token: ${{ secrets.HOMEBREW_TAP_TOKEN }}
    branch: bump-nice-${{ env.VERSION }}
    title: "nice ${{ env.VERSION }}"
    commit-message: "nice ${{ env.VERSION }}"
```

Auto-merge that PR once CI (on the tap side, `brew test-bot` via Homebrew's reusable workflow) passes.

### Phase 5 — Update the main repo's README + install flow

1. Replace the "Install" section of `README.md` with Homebrew as the default path, keeping `scripts/install.sh` as a secondary "build from source" option for contributors:
   ```sh
   brew install --cask Nick-Anderssohn/nice/nice
   ```
2. Add a "Releases" section linking to GitHub Releases and noting Apple Silicon + Intel support (if both — the current build is universal by default unless we've restricted it; verify during Phase 1).
3. Drop the `xattr -dr com.apple.quarantine` line from `scripts/install.sh` once notarization is live — that workaround exists specifically because of ad-hoc signing, and it becomes unnecessary (and misleading) once builds are notarized. Leave `install.sh` itself in place for from-source builds; just remove the xattr call.

## Critical files

- `project.yml:63-79` — version + bundle ID (version fields get bumped by release script).
- `scripts/install.sh:122` — the `xattr -dr com.apple.quarantine` line to remove in Phase 5.
- `Resources/Nice.entitlements` — no changes needed; already hardened-runtime-compatible.
- New: `scripts/release.sh`, `scripts/ExportOptions.plist`, `.github/workflows/release.yml`.
- New repo: `Nick-Anderssohn/homebrew-nice` with `Casks/nice.rb`.

## Verification

**After Phase 1 (local release):**
- Produced zip unpacks to an app that launches cleanly with no Gatekeeper prompt when double-clicked from Downloads.
- `spctl --assess --verbose=4 Nice.app` prints `accepted, source=Notarized Developer ID`.
- `codesign --verify --deep --strict --verbose=2 Nice.app` exits 0.
- `xcrun stapler validate Nice.app` prints `The validate action worked!`.

**After Phase 2 (CI):**
- Pushing tag `v0.1.0` creates a GitHub Release with `Nice-0.1.0.zip` attached.
- Downloading that zip from the Release page on a clean Mac and launching it: no Gatekeeper friction.

**After Phase 3 (tap):**
- `brew tap Nick-Anderssohn/nice && brew install --cask nice` on a Mac that doesn't have Nice installed → `/Applications/Nice.app` appears, Spotlight finds it, it launches.
- `brew audit --cask --strict nice` reports no errors.
- `brew uninstall --cask nice` removes the app; `brew uninstall --cask --zap nice` also wipes `~/Library/Preferences/dev.nickanderssohn.nice.plist`.

**After Phase 4 (auto-bump):**
- Tagging `v0.1.1` in `nice` opens a PR in `homebrew-nice` within a minute, which after merge allows `brew upgrade --cask nice` to pick up the new version.

## Order of operations (recommended)

1. Complete the four Apple Developer prerequisites above. (Blocker — nothing ships without this.)
2. Build `scripts/release.sh` and run it locally end-to-end for 0.1.0. Iterate until the four verification checks above pass. Do **not** tag a GitHub release yet.
3. Create `Nick-Anderssohn/homebrew-nice` and a hand-written `Casks/nice.rb` using the locally-notarized artifact (upload it to a GitHub Release manually for this one-time bootstrap). Verify `brew install --cask` works.
4. Wire up `.github/workflows/release.yml`. Tag `v0.1.1` as a real end-to-end CI test; confirm the release + auto-bump PR both appear.
5. Update the README and remove the quarantine workaround in `install.sh`.
