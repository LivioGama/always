#ifndef apple_intelligence_bridge_h
#define apple_intelligence_bridge_h

#ifdef __cplusplus
extern "C" {
#endif

typedef struct {
    char* response;
    int success; /* 0 for failure, 1 for success */
    char* error_message; /* Only valid when success = 0 */
} AppleLLMResponse;

/* Check if Apple Intelligence is available on the device */
int is_apple_intelligence_available(void);

/* Process text using Apple's on-device LLM with separate system prompt and user content */
AppleLLMResponse* process_text_with_system_prompt_apple(const char* system_prompt, const char* user_content, int max_tokens);

/* Same, with explicit mode bits (1 = pooled prewarmed session,
 * 2 = guided generation). For the latency harness. */
AppleLLMResponse* process_text_with_system_prompt_apple_mode(const char* system_prompt, const char* user_content, int max_tokens, int mode);

/* Create (and prewarm) the next session for `system_prompt` in the
 * background so the following process call skips session setup and
 * instruction prefill. Non-blocking. */
void apple_intelligence_prewarm(const char* system_prompt);

/* Free memory allocated by the Apple LLM response */
void free_apple_llm_response(AppleLLMResponse* response);

#ifdef __cplusplus
}
#endif

#endif /* apple_intelligence_bridge_h */
