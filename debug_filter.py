text = "No, sorry, I didn't mean to send this. So can you undo what we just said? And it actually makes me think that maybe you should also assess what is being said and not necessarily systematically add an entry. And then actually what I meant is that the problems that I currently have is that Iris looks disconnected, so it looks like it didn't run with the Gemini API key."

import re

# Simulate the Rust normalization
normalized = text.strip().strip(".,!?").lower()
print(f"Normalized: {normalized[:100]}...")

# Extract alphabetic only
alpha_only = ''.join(c for c in normalized if c.isalpha())
print(f"Alpha only length: {len(alpha_only)}")

# Vowel ratio check
vowels = sum(1 for c in alpha_only if c in 'aeiou')
vowel_ratio = vowels / len(alpha_only) if len(alpha_only) > 0 else 0
print(f"Vowels: {vowels}, Total: {len(alpha_only)}, Ratio: {vowel_ratio:.3f}")

if len(alpha_only) >= 40 and vowel_ratio < 0.15:
    print("❌ FAILED: Vowel ratio too low!")
else:
    print("✅ PASSED: Vowel ratio OK")

# Consonant cluster check
cluster_count = 0
current_cluster_len = 0
for c in alpha_only:
    if c in 'aeiou':
        if current_cluster_len >= 5:
            cluster_count += 1
            print(f"Found consonant cluster of length {current_cluster_len}")
        current_cluster_len = 0
    else:
        current_cluster_len += 1

if current_cluster_len >= 5:
    cluster_count += 1
    print(f"Final consonant cluster of length {current_cluster_len}")

print(f"Consonant clusters (5+): {cluster_count}")
if cluster_count >= 3:
    print("❌ FAILED: Too many consonant clusters!")
else:
    print("✅ PASSED: Consonant clusters OK")

# Check long words without vowels
words = normalized.split()
long_words = [w for w in words if len([c for c in w if c.isalpha()]) >= 4]
no_vowel_words = [w for w in long_words if not any(c in 'aeiou' for c in w)]

print(f"Long words (4+ chars): {len(long_words)}")
print(f"Long words without vowels: {len(no_vowel_words)}")
print(f"No-vowel words: {no_vowel_words}")

if len(long_words) >= 3:
    ratio = len(no_vowel_words) / len(long_words)
    print(f"No-vowel ratio: {ratio:.3f}")
    if ratio > 0.5:
        print("❌ FAILED: Too many words without vowels!")
    else:
        print("✅ PASSED: Word vowel distribution OK")
