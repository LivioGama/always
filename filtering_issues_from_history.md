# Filtering Issues from Conversation History

Analysis of conversation history from `~/.claude/history.jsonl` and Always logs to identify filtering problems.

## False Positives (Incorrectly Filtered - Should NOT have been filtered)

### 1. Long valid conversation filtered as Gibberish
**Text:** "No, sorry, I didn't mean to send this. So can you undo what we just said? And it actually makes me think that maybe you should also assess what is being said and not necessarily systematically add an entry. And then actually what I meant is that the problems that I currently have is that Iris looks disconnected, so it looks like it didn't run with the Gemini API key."

**Filtered as:** Gibberish: Unpronounceable text or keyboard mashing

**Issue:** This is valid natural conversation about technical problems, not gibberish. The gibberish detection is too aggressive.

**Timestamp:** 1777596888145

---

### 2. Valid single word "Yes" filtered as Sound effect
**Text:** "Yes."

**Filtered as:** Sound effect: Repetitive sound pattern or very short utterance

**Issue:** "Yes" is a valid single-word response that should be accepted. The onomatopoeia/sound detection is incorrectly flagging valid words.

**Timestamp:** 1777603755144

---

### 3. Valid technical question filtered as Gibberish
**Text:** "So I actually implemented it somewhere else, not in this program. So can you actually tell me what the implementation is worth by checking it? But start by actually running the test and tell me if you think the test is reliable."

**Filtered as:** Gibberish: Unpronounceable text or keyboard mashing

**Issue:** Valid technical conversation about implementation and testing, incorrectly flagged as gibberish.

**Timestamp:** 04:55:31

---

### 4. Valid technical question filtered as Gibberish
**Text:** "Can you implement a simple frontend with a studio in order to generate images with different LLM models?"

**Filtered as:** Gibberish: Unpronounceable text or keyboard mashing

**Issue:** Valid technical question about frontend implementation, incorrectly flagged as gibberish.

**Timestamp:** 05:00:01

---

### 5. Valid technical question filtered as Gibberish
**Text:** "Considering I want to do everything I can to keep Web Speech API, do you think it will be possible to use official and pay or maybe actually just use voice activation detection to start the session at this moment?"

**Filtered as:** Gibberish: Unpronounceable text or keyboard mashing

**Issue:** Valid technical question about Web Speech API, incorrectly flagged as gibberish.

**Timestamp:** 05:08:05

---

### 6. Valid technical question filtered as Gibberish
**Text:** "But let's say I'm ready to pay. Is there a version of web speech that is not a workaround official by Google?"

**Filtered as:** Gibberish: Unpronounceable text or keyboard mashing

**Issue:** Valid question about Web Speech API pricing, incorrectly flagged as gibberish.

**Timestamp:** 05:09:42

---

### 7. Valid technical question filtered as Gibberish
**Text:** "Can you compare the price of Google Cloud Speech-to-Text with the price of Grok Waysperl Arch Free Turbo?"

**Filtered as:** Gibberish: Unpronounceable text or keyboard mashing

**Issue:** Valid pricing comparison question, incorrectly flagged as gibberish.

**Timestamp:** 05:11:18

---

### 8. Valid word "perception" filtered as Repetitive
**Text:** "perception"

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid English word incorrectly filtered.

**Timestamp:** 05:27:07

---

### 9. Valid word "Token" filtered as Repetitive
**Text:** "Token."

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid technical term incorrectly filtered.

**Timestamp:** 06:03:03

---

### 10. Valid word "Next" filtered as Repetitive
**Text:** "Next"

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid command word incorrectly filtered.

**Timestamp:** 05:06:55

---

### 11. Valid word "Nothing" filtered as Repetitive
**Text:** "Nothing."

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid response word incorrectly filtered.

**Timestamp:** 06:37:24

---

### 12. Valid name "Clemente" filtered as Repetitive
**Text:** "Clemente"

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid name incorrectly filtered.

**Timestamp:** 06:39:33

---

### 13. Valid word "Yammer" filtered as Repetitive
**Text:** "Yammer."

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid word (could be Yammer platform or regular word) incorrectly filtered.

**Timestamp:** 06:06:23

---

### 14. Valid name "Bogba" filtered as Repetitive
**Text:** "Bogba."

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid name incorrectly filtered.

**Timestamp:** 05:17:17

---

### 15. Valid name "Bequiera" filtered as Repetitive
**Text:** "Bequiera."

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid name incorrectly filtered.

**Timestamp:** 04:32:20

---

### 16. Valid name "T-Series" filtered as Repetitive
**Text:** "T-Series,"

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** Valid name (YouTube channel) incorrectly filtered.

**Timestamp:** 05:06:44

---

### 17. Valid word "TO" filtered as Sound effect
**Text:** "TO"

**Filtered as:** Sound effect: Repetitive sound pattern or very short utterance

**Issue:** Valid word incorrectly filtered.

**Timestamp:** 04:37:49

---

### 18. Valid filler "Uhh..." filtered as Sound effect
**Text:** "Uhh..."

**Filtered as:** Sound effect: Repetitive sound pattern or very short utterance

**Issue:** While fillers should generally be filtered, this is a valid conversational filler that might be acceptable in some contexts.

**Timestamp:** 04:28:31

---

### 19. Valid word "Ugh" filtered as Sound effect
**Text:** "Ugh."

**Filtered as:** Sound effect: Repetitive sound pattern or very short utterance

**Issue:** Valid expression of frustration, might be acceptable in some contexts.

**Timestamp:** 05:58:53

---

### 20. Valid word "Burp" filtered as Repetitive
**Text:** "Burp."

**Filtered as:** Repetitive: Repetitive or low-diversity content

**Issue:** While "burp" is a sound, it could also be a valid word (e.g., Burp Suite security tool).

**Timestamp:** 06:28:49

---

## False Negatives (Should have been filtered but weren't)

### 1. "Thank you for watching!" (6 instances)
**Text:** "Thank you for watching!"

**Issue:** Classic video artifact that should be rejected. This is the exact pattern the filter should catch.

**Occurrences:** Multiple timestamps including 1777310214943, 1777312288497, 1777331201431, 1777332260964, 1777338430903

---

### 2. "Subtitles by the Amara.org community" (5 instances)
**Text:** "Subtitles by the Amara.org community"

**Issue:** Transcription service watermark that should be rejected as video artifact.

**Occurrences:** Multiple timestamps including 1777472606988, 1777475601995, 1777475770278

**User note:** "Subtitles by the Amara.org community => should have been filterered" (Timestamp: 1777603625970)

---

### 3. Gibberish: "garantgamescom. senten Walking,"
**Text:** "garantgamescom. senten Walking,"

**Issue:** Appears to be gibberish or corrupted text that should have been filtered.

**Timestamp:** Found in Always logs at 07:17:11

---

### 4. Unclear/gibberish: "50FC World"
**Text:** "50FC World"

**Issue:** Unclear meaning, possibly gibberish that should have been filtered.

**Timestamp:** Found in Always logs at 07:37:11

---

### 5. Video content: "Microsoft. Zed shifted to Carlsbad when he was 12 years old."
**Text:** "Microsoft. Zed shifted to Carlsbad when he was 12 years old."

**Issue:** Appears to be video narrative content that should have been filtered as video artifact.

**Timestamp:** Found in Always logs at 06:29:01

---

### 6. Video intro/outro: "Establishing, funding. . Maya University, CUNY A Creative Hello again and thank you to Vüterpikvic Jamesilian American University, spectrum and complementarity,"
**Text:** "Establishing, funding. . Maya University, CUNY A Creative Hello again and thank you to Vüterpikvic Jamesilian American University, spectrum and complementarity,"

**Issue:** Classic video intro/outro content with sponsor/organization mentions that should be filtered.

**Timestamp:** Found in Always logs at 06:28:45

---

## Summary

### Root Causes

1. **Gibberish detection too aggressive** - Valid long sentences and technical questions are being incorrectly flagged as gibberish (5+ cases)
2. **Onomatopoeia/sound detection too aggressive** - Valid single words like "Yes", "TO", "Token" are being incorrectly filtered (3+ cases)
3. **Repetitive/degeneracy detection too aggressive** - Valid words and names are being incorrectly filtered (8+ cases: "perception", "Token", "Next", "Nothing", "Clemente", "Yammer", "Bogba", "Bequiera", "T-Series", "Burp")
4. **Video artifact detection incomplete** - Classic patterns like "Thank you for watching" and subtitle watermarks are getting through
5. **Video content detection missing** - Video narrative and intro/outro content is not being caught

### Recommendations

1. **Fix gibberish detection** - Be much more conservative with gibberish classification. Check for valid sentence structure, proper grammar, and meaningful content before flagging as gibberish
2. **Fix onomatopoeia detection** - Ensure valid single words (especially common responses like "Yes", "No", "Next", "Token") are in a comprehensive allowlist
3. **Fix repetitive/degeneracy detection** - Add comprehensive allowlist for valid single words, names, and technical terms. The current detection is flagging too many legitimate words
4. **Strengthen video artifact detection** - The recent LLM prompt improvements should help, but ensure fallback rules are comprehensive
5. **Add video content pattern detection** - Consider patterns for video narratives, sponsor mentions, and intro/outro content

### Recent Improvements Made

- Enhanced LLM prompt with comprehensive video artifact patterns (subscribe prompts, subtitle metadata, transcription services)
- Improved fallback logic with pattern matching for video artifacts and conversational fillers
- Added "thank you for your attention" to video artifact patterns

These improvements should address the false negative cases (video artifacts getting through), but the false positive cases (20+ instances of valid content being filtered) still need significant attention. The filter is being overly aggressive across multiple detection mechanisms.
