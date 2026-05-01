text = "No, sorry, I didn't mean to send this. So can you undo what we just said? And it actually makes me think that maybe you should also assess what is being said and not necessarily systematically add an entry. And then actually what I meant is that the problems that I currently have is that Iris looks disconnected, so it looks like it didn't run with the Gemini API key."

normalized = text.strip().strip(".,!?").lower()
alpha_only = ''.join(c for c in normalized if c.isalpha())

# Test with improved thresholds (6+ consonants, 5+ clusters)
cluster_count = 0
current_cluster_len = 0
for c in alpha_only:
    if c in 'aeiou':
        if current_cluster_len >= 6:  # New threshold
            cluster_count += 1
            print(f"Found consonant cluster of length {current_cluster_len}")
        current_cluster_len = 0
    else:
        current_cluster_len += 1

if current_cluster_len >= 6:  # New threshold
    cluster_count += 1

print(f"Consonant clusters (6+): {cluster_count}")
if cluster_count >= 5:  # New threshold
    print("❌ FAILED: Too many consonant clusters!")
else:
    print("✅ PASSED: Consonant clusters OK - text should NOT be filtered!")
