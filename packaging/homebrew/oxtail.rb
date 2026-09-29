cask "oxtail" do
  version "@VERSION@"
  sha256 "@SHA256_MACOS_DMG@"

  url "https://github.com/patricksindelka/OxTail/releases/download/v#{version}/oxtail-#{version}-macos-universal.dmg"
  name "OxTail"
  desc "Fast, portable, real-time log viewer"
  homepage "https://github.com/patricksindelka/OxTail"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on macos: :big_sur

  app "OxTail.app"
  binary "#{appdir}/OxTail.app/Contents/MacOS/oxtail"

  zap trash: "~/Library/Application Support/OxTail"
end
