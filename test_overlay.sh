#!/bin/bash

# Test overlay state transitions
STATE_FILE="$HOME/.config/always/state.json"

echo "Testing overlay state transitions..."

# Test 1: Set paused state
echo "Test 1: Setting paused state"
cat > "$STATE_FILE" << EOF
{
  "listening": false,
  "processing": false,
  "transcribing": false,
  "paused": true,
  "auto_enter": false,
  "last_transcript": null,
  "last_updated": $(date +%s),
  "version": 1
}
EOF
echo "State updated - overlay should show orange (paused)"
sleep 2

# Test 2: Set listening state
echo "Test 2: Setting listening state"
cat > "$STATE_FILE" << EOF
{
  "listening": true,
  "processing": false,
  "transcribing": false,
  "paused": false,
  "auto_enter": false,
  "last_transcript": null,
  "last_updated": $(date +%s),
  "version": 2
}
EOF
echo "State updated - overlay should show red pulsing (listening)"
sleep 2

# Test 3: Set transcribing state
echo "Test 3: Setting transcribing state"
cat > "$STATE_FILE" << EOF
{
  "listening": false,
  "processing": false,
  "transcribing": true,
  "paused": false,
  "auto_enter": false,
  "last_transcript": null,
  "last_updated": $(date +%s),
  "version": 3
}
EOF
echo "State updated - overlay should show purple pulsing (transcribing)"
sleep 2

# Test 4: Set auto-enter state
echo "Test 4: Setting auto-enter state"
cat > "$STATE_FILE" << EOF
{
  "listening": false,
  "processing": false,
  "transcribing": false,
  "paused": false,
  "auto_enter": true,
  "last_transcript": null,
  "last_updated": $(date +%s),
  "version": 4
}
EOF
echo "State updated - overlay should show green (auto-enter)"
sleep 2

# Test 5: Clear all states (hidden)
echo "Test 5: Clearing all states"
cat > "$STATE_FILE" << EOF
{
  "listening": false,
  "processing": false,
  "transcribing": false,
  "paused": false,
  "auto_enter": false,
  "last_transcript": "Test completed",
  "last_updated": $(date +%s),
  "version": 5
}
EOF
echo "State updated - overlay should be hidden"

echo "Overlay state testing complete!"