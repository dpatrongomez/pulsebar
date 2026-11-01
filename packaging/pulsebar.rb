# Homebrew cask (put in a tap: Casks/pulsebar.rb). Fill url/sha256 from packaging/bundle.sh output.
cask "pulsebar" do
  version "0.1.0"
  sha256 "REPLACE_WITH_SHA256_FROM_BUNDLE_SH"

  url "https://github.com/dpatrongomez/pulsebar/releases/download/v#{version}/PulseBar-#{version}.zip"
  name "PulseBar"
  desc "App-centric activity monitor for developers (MCP + CLI)"
  homepage "https://pulsebar.app"

  depends_on macos: ">= :sequoia"

  app "PulseBar.app"
  # Same executable: run from a terminal it is the CLI.
  binary "#{appdir}/PulseBar.app/Contents/MacOS/PulseBar", target: "pulsebar"

  uninstall quit: "app.pulsebar.PulseBar"

  zap trash: [
    "~/Library/Application Support/PulseBar",
    "~/Library/Caches/PulseBar",
  ]
end
