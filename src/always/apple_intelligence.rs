//! Safe Rust wrapper around the Apple Intelligence Swift bridge.
//!
//! The Swift side (`swift/apple_intelligence.swift`) uses FoundationModels
//! (`LanguageModelSession` + `SystemLanguageModel`) to run on-device LLM
//! inference. The bridge exposes three C functions via `@_cdecl`; this
//! module wraps them in safe Rust types.
//!
//! On non-Apple-Intelligence builds (stub Swift file), the functions still
//! exist but always report unavailable — so callers can safely call
//! `check_availability()` and get `false`.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int};

#[repr(C)]
struct AppleLLMResponse {
    response: *mut c_char,
    success: c_int,
    error_message: *mut c_char,
}

unsafe extern "C" {
    fn is_apple_intelligence_available() -> c_int;
    fn process_text_with_system_prompt_apple(
        system_prompt: *const c_char,
        user_content: *const c_char,
        max_tokens: i32,
    ) -> *mut AppleLLMResponse;
    fn free_apple_llm_response(response: *mut AppleLLMResponse);
}

/// True when Apple Intelligence is available on this device and build.
pub fn check_availability() -> bool {
    unsafe { is_apple_intelligence_available() == 1 }
}

/// Process text with Apple Intelligence using a separate system prompt
/// and user content. Returns the cleaned text on success.
///
/// This is a **blocking** call — the Swift side uses a semaphore to bridge
/// the async FoundationModels API into a synchronous C call. The caller
/// should run this on a thread that can afford to block (e.g. within
/// `tokio::task::spawn_blocking`).
pub fn process_text_with_system_prompt(
    system_prompt: &str,
    user_content: &str,
    max_tokens: i32,
) -> Result<String, String> {
    let system_cstr = CString::new(system_prompt).map_err(|e| e.to_string())?;
    let user_cstr = CString::new(user_content).map_err(|e| e.to_string())?;

    let response_ptr = unsafe {
        process_text_with_system_prompt_apple(
            system_cstr.as_ptr(),
            user_cstr.as_ptr(),
            max_tokens,
        )
    };

    if response_ptr.is_null() {
        return Err("Null response from Apple LLM".to_string());
    }

    let result = unsafe {
        let response = &*response_ptr;
        if response.success == 1 {
            if response.response.is_null() {
                Ok(String::new())
            } else {
                let c_str = CStr::from_ptr(response.response);
                Ok(c_str.to_string_lossy().into_owned())
            }
        } else {
            let error_msg = if !response.error_message.is_null() {
                CStr::from_ptr(response.error_message).to_string_lossy().into_owned()
            } else {
                "Unknown error".to_string()
            };
            Err(error_msg)
        }
    };

    unsafe { free_apple_llm_response(response_ptr) };

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn probe_live_inference() {
        let available = check_availability();
        println!("\navailable: {available}");
        let out = process_text_with_system_prompt(
            "You are a grammar cleaner. Fix grammar, remove filler words. Return only the cleaned text.",
            "hey so uhm i want to go to the store tomorrow but i dont have time",
            80,
        );
        match out {
            Ok(t) => println!("OK: {t}"),
            Err(e) => println!("ERR: {e}"),
        }
    }

    #[test]
    fn test_availability() {
        let available = check_availability();
        println!("Apple Intelligence available: {}", available);
        // Just verify it doesn't crash — the result depends on the device.
    }
}
