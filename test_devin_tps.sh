#!/bin/bash

# Token per second test for SWE 1.6 Fast (Devin)
# Measures tokens per second by calling devin CLI externally

PROMPT="write a detailed explanation of how neural networks work, including backpropagation and gradient descent"

echo "Testing Devin tokens per second..."
echo "Prompt: '$PROMPT'"
echo ""

START_TIME=$(date +%s%N)
OUTPUT=$(devin -p "$PROMPT" 2>&1)
END_TIME=$(date +%s%N)

DURATION_MS=$(( (END_TIME - START_TIME) / 1000000 ))
DURATION_SEC=$(echo "scale=3; $DURATION_MS / 1000" | bc)

# Estimate tokens (rough approximation: ~4 characters per token)
CHAR_COUNT=${#OUTPUT}
TOKEN_ESTIMATE=$((CHAR_COUNT / 4))

TOKENS_PER_SEC=$(echo "scale=2; $TOKEN_ESTIMATE / $DURATION_SEC" | bc)

echo ""
echo "=== Results ==="
echo "Duration: ${DURATION_MS}ms (${DURATION_SEC}s)"
echo "Output characters: $CHAR_COUNT"
echo "Estimated tokens: $TOKEN_ESTIMATE"
echo "Tokens per second: $TOKENS_PER_SEC"