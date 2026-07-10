//! Quick test script to verify llama.cpp CLI works with Qwen2.5 model
use std::process::Command;

fn main() {
    let model_path = "/tmp/qwen2.5-3b-instruct-q4_k_m.gguf";
    
    println!("Testing llama.cpp CLI inference...");
    
    // Check if model exists
    if !std::path::Path::new(model_path).exists() {
        eprintln!("Model not found at {}. Downloading...", model_path);
        let download = Command::new("curl")
            .arg("-L")
            .arg("-o")
            .arg(model_path)
            .arg("https://huggingface.co/Qwen/Qwen2.5-3B-Instruct-GGUF/resolve/main/qwen2.5-3b-instruct-q4_k_m.gguf")
            .status();
        
        if !download.success() {
            eprintln!("Download failed");
            return;
        }
    }
    
    // Test inference
    let system_prompt = "You are a helpful assistant. Correct the grammar of the input text.";
    let user_text = "I have a idea about kubernetes and want to deploy it to production.";
    
    let prompt = format!(
        "<|im_start|>system\n{}\n<|im_end|>\n<|im_start|>user\n{}\n<|im_end|>\n<|im_start|>assistant\n",
        system_prompt, user_text
    );
    
    println!("Running inference...");
    
    let output = Command::new("llama-cli")
        .arg("-m")
        .arg(model_path)
        .arg("-p")
        .arg(&prompt)
        .arg("-n")
        .arg("500")
        .arg("--temp")
        .arg("0.3")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output();
    
    match output {
        Ok(result) => {
            if result.status.success() {
                let output_text = String::from_utf8_lossy(&result.stdout);
                let cleaned = output_text
                    .strip_prefix("<|im_start|>assistant\n")
                    .unwrap_or(&output_text)
                    .strip_suffix("<|im_end|>")
                    .unwrap_or(&output_text)
                    .trim();
                println!("SUCCESS! Output: {}", cleaned);
            } else {
                eprintln!("llama-cli failed with exit code: {:?}", result.status.code());
            }
        }
        Err(e) => {
            eprintln!("Failed to run llama-cli: {}", e);
            eprintln!("Make sure llama.cpp is installed: brew install llama.cpp");
        }
    }
}