#!/bin/bash
# Test STT pipeline with a short recording
# Records 5 seconds of audio and runs it through the full transcription pipeline

set -e

DURATION=${1:-5}
OUTPUT_FILE="/tmp/stt_test_$(date +%s).wav"

echo "🎙️  STT Pipeline Tester"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Recording for $DURATION seconds..."
echo "Speak clearly into your microphone."
echo ""

# Record audio using ffmpeg with MacBook Pro Microphone (quieter output)
# Format: -i ':audio_device_index' for audio-only, or 'video:audio' for both
ffmpeg -f avfoundation -i ':2' -t "$DURATION" "$OUTPUT_FILE" 2>/dev/null

if [ ! -f "$OUTPUT_FILE" ]; then
    echo "❌ Failed to record audio"
    exit 1
fi

echo ""
echo "✅ Recording saved to: $OUTPUT_FILE"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""

# Run the test binary
cargo run --bin test_stt -- "$OUTPUT_FILE"

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "📁 Raw audio file: $OUTPUT_FILE"
echo "💡 Tip: Try the phrase: 'Why is it defined in 2 places?'"
