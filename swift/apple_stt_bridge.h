#ifndef apple_stt_bridge_h
#define apple_stt_bridge_h

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    char* text;
    int success; /* 0 for failure, 1 for success */
    char* error_message; /* Only valid when success = 0 */
} AppleSttResponse;

/* Check if on-device Apple Speech (SFSpeechRecognizer) is available */
int is_apple_stt_available(void);

/* Transcribe a WAV file at wav_path using Apple's speech engine.
 * lang is an ISO 639-1 hint (e.g. "en") or NULL/empty for default.
 * phrases is a newline-joined list of domain terms for contextual biasing
 * (AnalysisContext.contextualStrings), or NULL/empty for none. */
AppleSttResponse* transcribe_wav_with_apple_stt(const char* wav_path, const char* lang, const char* phrases);

/* Free memory allocated by an Apple STT response */
void free_apple_stt_result(AppleSttResponse* response);

#ifdef __cplusplus
}
#endif

#endif /* apple_stt_bridge_h */
