#ifndef apple_stt_bridge_h
#define apple_stt_bridge_h

#include <stddef.h>

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

/* Incremental streaming session (macOS 26+, SpeechAnalyzer push API).
 * Sessions are opaque pointers; finish/cancel consume them. */

/* 1 when incremental streaming can run on this device (macOS 26+ and
 * speech recognition available/authorized), else 0. */
int apple_stt_stream_supported(void);

/* Start a streaming session. lang is an ISO 639-1 hint or NULL/empty;
 * phrases is a newline-joined bias list or NULL/empty. Returns an opaque
 * session pointer, or NULL when streaming cannot start now (not authorized,
 * unsupported locale, or the on-device model is not installed yet — the
 * one-shot path covers those cases). Blocks up to ~4 s on async setup. */
void* apple_stt_stream_start(const char* lang, const char* phrases);

/* Push count float32 samples (16 kHz mono, [-1,1]) into the session — the
 * Swift side converts to the Int16 PCM the recognizer worker requires.
 * Returns a strdup'd cumulative transcript decoded so far (possibly empty),
 * or NULL on failure. Caller frees with free_apple_stt_string. */
char* apple_stt_stream_push(void* session, const float* samples, size_t count);

/* Close input, flush the analyzer, return the final transcript. Consumes
 * the session pointer — do not reuse or free it afterwards. */
AppleSttResponse* apple_stt_stream_finish(void* session);

/* Abandon a session without a result. Consumes the session pointer. */
void apple_stt_stream_cancel(void* session);

/* Free a string returned by apple_stt_stream_push */
void free_apple_stt_string(char* s);

#ifdef __cplusplus
}
#endif

#endif /* apple_stt_bridge_h */
