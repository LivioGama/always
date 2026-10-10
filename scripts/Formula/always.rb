class Always < Formula
  desc "Always-on speech-to-text daemon — hands-free voice dictation on macOS"
  homepage "https://github.com/LivioGama/always"

  # The version is bumped by the homebrew-bump-formula action on every release tag.
  version "0.0.1"

  on_macos do
    # The release workflow produces always-{version}.dmg containing Always.app.
    # The DMG is the same for arm64 and x86_64 (Universal binary).
    url "https://github.com/LivioGama/always/releases/download/v0.0.1/always-0.0.1.dmg"
    sha256 "PLACEHOLDER_UNIVERSAL"
  end

  def install
    dmg_path = cached_download
    tmpdir = Dir.mktmpdir
    # Mount the DMG and extract the app bundle
    system "hdiutil", "attach", "-nobrowse", "-quiet", dmg_path, "-mountpoint", tmpdir
    app_dir = Dir.entries(tmpdir).select { |e| e.end_with?(".app") }.first
    app_path = File.join(tmpdir, app_dir) if app_dir

    if app_path && Dir.exist?(app_path)
      app_name = File.basename(app_path, ".app")
      # Install app bundle
      prefix.install(app_path) => app_name
      
      # Install the daemon binary from inside the app bundle
      binary_path = File.join(app_path, "Contents", "MacOS", app_name)
      if File.exist?(binary_path)
        bin.install binary_path => app_name
      end
    else
      # Fallback: copy the whole app and extract binary separately
      system "hdiutil", "detach", tmpdir, "-quiet"
      FileUtils.rm_rf(tmpdir)
      return # Let homebrew handle app installation via appcast
    end
    
    # Unmount and cleanup
    system "hdiutil", "detach", tmpdir, "-quiet"
    FileUtils.rm_rf(tmpdir)
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
