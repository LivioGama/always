text = "No, sorry, I didn't mean to send this. So can you undo what we just said? And it actually makes me think that maybe you should also assess what is being said and not necessarily systematically add an entry. And then actually what I meant is that the problems that I currently have is that Iris looks disconnected, so it looks like it didn't run with the Gemini API key."

# Extract the consonant clusters
normalized = text.strip().strip(".,!?").lower()
alpha_only = ''.join(c for c in normalized if c.isalpha())

print("Finding consonant clusters of 5+ characters:")
print(f"Text: {alpha_only}")
print()

current_cluster = ""
position = 0
for i, c in enumerate(alpha_only):
    if c in 'aeiou':
        if len(current_cluster) >= 5:
            start_pos = position - len(current_cluster)
            end_pos = position
            context_start = max(0, start_pos - 10)
            context_end = min(len(alpha_only), end_pos + 10)
            context = alpha_only[context_start:context_end]
            print(f"Cluster: '{current_cluster}' (length {len(current_cluster)})")
            print(f"Context: ...{context}...")
            print(f"Position: {start_pos}-{end_pos}")
            print()
        current_cluster = ""
    else:
        current_cluster += c
    position = i + 1

# Check final cluster
if len(current_cluster) >= 5:
    start_pos = position - len(current_cluster)
    end_pos = position
    context_start = max(0, start_pos - 10)
    context_end = min(len(alpha_only), end_pos + 10)
    context = alpha_only[context_start:context_end]
    print(f"Final Cluster: '{current_cluster}' (length {len(current_cluster)})")
    print(f"Context: ...{context}...")
