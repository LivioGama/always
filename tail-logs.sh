#!/bin/bash
# Always voice-to-text daemon log viewer
# This script tails the Always daemon log file with proper path escaping

LOG_PATH="/Users/livio/Library/Application Support/always/always.log"

# Check if log file exists
if [ ! -f "$LOG_PATH" ]; then
    echo "❌ Always log file not found at: $LOG_PATH"
    echo "💡 Make sure the Always daemon has been started at least once."
    exit 1
fi

echo "📋 Tailing Always daemon logs..."
echo "📁 Log file: $LOG_PATH"
echo "🔄 Press Ctrl+C to stop"
echo "$(printf '%.0s─' {1..60})"

# Tail the log file with follow mode
tail -f "$LOG_PATH"