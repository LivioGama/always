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

/* ---- Streaming recognition (live partials) ---- */

/* Start a live streaming session. lang is an ISO 639-1 hint or NULL;
 * phrases is a newline-joined contextual-bias list or NULL.
 * Returns an opaque session handle or NULL when recognition is
 * unavailable/unauthorized (caller falls back to the file path). */
void* apple_stt_stream_start(const char* lang, const char* phrases);

/* Feed 16 kHz mono signed-LE i16 samples into the session. */
void apple_stt_stream_feed(void* handle, const short* samples, int count);

/* Latest partial text as a freshly-allocated C string, or NULL when
 * unchanged since `seen` (pass the previous result to dedupe). */
char* apple_stt_stream_partial(void* handle, const char* seen);

/* End the audio stream and block up to timeout_ms for the final text.
 * Returns a freshly-allocated C string or NULL when nothing was
 * recognized. Falls back to the best partial on timeout. */
char* apple_stt_stream_finish(void* handle, int timeout_ms);

/* Cancel a session and release it. */
void apple_stt_stream_free(void* handle);

/* Free strings returned by apple_stt_stream_partial / _finish. */
void free_apple_stt_string(char* ptr);

#ifdef __cplusplus
}
#endif

#endif /* apple_stt_bridge_h */
