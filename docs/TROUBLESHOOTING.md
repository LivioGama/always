# 🔧 Troubleshooting

## Daemon won't start?

```bash
always status                              # Check if running
tail -f ~/.config/always/always.log        # View logs
always run                                 # Run foreground to see errors
```

## Audio issues?

```bash
# Install SoX
macOS:   brew install sox
Ubuntu:  apt install sox
Arch:    pacman -S sox

# Test microphone
rec -t wav test.wav trim 0 3
```

## API issues?

```bash
always config show                          # Verify API key
curl -H "Authorization: Bearer $GROQ_API_KEY" https://api.groq.com/openai/v1/models
```

## Permissions

### macOS
- Grant microphone access when prompted
- System Settings → Privacy & Security → Accessibility → Enable Always

### Linux
- Ensure user is in `audio` group
- Check PulseAudio/ALSA permissions

### Windows
- Grant microphone permissions in Privacy Settings
- May need administrator for global hotkeys
