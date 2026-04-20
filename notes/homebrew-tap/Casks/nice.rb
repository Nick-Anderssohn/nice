# Starter file for the Nick-Anderssohn/homebrew-nice tap.
#
# Bootstrap (do this once, after scripts/release.sh produces Nice-0.1.0.zip
# locally and you've uploaded that zip to a v0.1.0 GitHub Release):
#   1. Create https://github.com/Nick-Anderssohn/homebrew-nice (empty, MIT).
#   2. Copy this file to Casks/nice.rb in that repo.
#   3. Replace the sha256 placeholder below with `shasum -a 256` from the
#      release.sh output.
#   4. Commit, push, then verify on a clean Mac:
#        brew tap Nick-Anderssohn/nice
#        brew install --cask nice
#        brew audit --cask --strict nice
#
# After bootstrap, .github/workflows/release.yml opens a PR against this
# file on every v*.*.* tag — you never hand-edit version/sha again.

cask "nice" do
  version "0.1.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

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
