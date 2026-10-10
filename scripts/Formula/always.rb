class Always < Formula
  desc "Always-on speech-to-text daemon — hands-free voice dictation on macOS"
  homepage "https://github.com/LivioGama/always"

  # For the homebrew bump action to work, we use version + url pattern.
  # The workflow bumps this on every release tag.
  version "0.0.1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/LivioGama/always/releases/download/#{version}/always-#{version}-macos-arm64"
      sha256 "PLACEHOLDER"
    else
      url "https://github.com/LivioGama/always/releases/download/#{version}/always-#{version}-macos-x86_64"
      sha256 "PLACEHOLDER"
    end
  end

  depends_on "mas" => :optional # For the GUI app (optional cask path)

  def install
    bin.install "always" => "always"
  end

  plist_options startup: true

  def plist
    <<~PLIST
      <?xml version="1.0" encoding="UTF-8"?>
      <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
      <plist version="1.0">
      <dict>
        <key>Label</key>
        <string>#{plist_name}</string>
        <key>ProgramArguments</key>
        <array>
          <string>#{opt_bin}/always</string>
          <string>run</string>
        </array>
        <key>RunAtLoad</key>
        <true/>
        <key>KeepAlive</key>
        <true/>
        <key>StandardErrorPath</key>
        <string>#{HOMEBREW_PREFIX}/var/log/always.log</string>
        <key>StandardOutPath</key>
        <string>#{HOMEBREW_PREFIX}/var/log/always.log</string>
        <key>EnvironmentVariables</key>
        <dict>
          <key>HOMEBREW_PREFIX</key>
          <string>#{HOMEBREW_PREFIX}</string>
        </dict>
      </dict>
      </plist>
    PLIST
  end

  test do
    system "#{bin}/always", "--help"
  end
end
