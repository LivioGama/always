use std::env;
use always::always::ai_filter::{AiFilter, create_ai_filter};
use always::always::AlwaysConfig;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("Usage: {} <text_to_test>", args[0]);
        println!("       {} --interactive  (for interactive mode)", args[0]);
        println!("       {} --batch <file>  (test lines from file)", args[0]);
        std::process::exit(1);
    }

    // Check for API key
    if std::env::var("GROQ_API_KEY").is_err() {
        println!("⚠️  WARNING: GROQ_API_KEY not set. Using fallback rules only.");
        println!("   Set GROQ_API_KEY to test full AI filtering capabilities with Groq/Llama.");
        println!();
    }

    if args[1] == "--interactive" {
        interactive_mode().await;
    } else if args[1] == "--batch" && args.len() > 2 {
        batch_mode(&args[2]).await;
    } else {
        let text = args[1..].join(" ");
        test_single_text(&text).await;
    }
}

async fn test_single_text(text: &str) {
    println!("🤖 AI-Powered Filter Test");
    println!("Testing: \"{}\"", text);
    println!("{}", "─".repeat(60));

    let config = AlwaysConfig::from_cli(
        "en".to_string(),
        30,
        0.5,
        false
    ).expect("Failed to create config");

    if let Some(ai_filter) = create_ai_filter(&config) {
        match ai_filter.evaluate_transcription(text, &config).await {
            Ok(result) => {
                println!("✨ AI EVALUATION RESULT:");
                println!("📝 Corrected Text: \"{}\"", result.corrected_text);
                println!("🎯 Confidence: {:.1}%", result.confidence_score * 100.0);
                println!("📊 Category: {:?}", result.filter_category.unwrap_or_else(||
                    always::always::ai_filter::FilterCategory::NaturalConversation));
                println!("💭 Reasoning: {}", result.reason);
                println!("{}", "─".repeat(60));

                if result.should_accept {
                    println!("✅ ACCEPTED: Text would be pasted");
                } else {
                    println!("❌ REJECTED: {}", result.reason);
                }
            }
            Err(e) => {
                println!("❌ AI Evaluation failed: {}", e);
                println!("🔄 Falling back to simple rules...");

                let result = ai_filter.evaluate_fallback(text);
                println!("📝 Fallback Result: \"{}\"", result.corrected_text);
                println!("💭 Reasoning: {}", result.reason);

                if result.should_accept {
                    println!("✅ ACCEPTED (fallback): Text would be pasted");
                } else {
                    println!("❌ REJECTED (fallback): {}", result.reason);
                }
            }
        }
    } else {
        println!("⚠️  No API key available. Using fallback rules only.");
        let dummy_filter = AiFilter::new("dummy".to_string(), None);
        let result = dummy_filter.evaluate_fallback(text);

        println!("📝 Fallback Result: \"{}\"", result.corrected_text);
        println!("💭 Reasoning: {}", result.reason);

        if result.should_accept {
            println!("✅ ACCEPTED (fallback): Text would be pasted");
        } else {
            println!("❌ REJECTED (fallback): {}", result.reason);
        }
    }
}

async fn interactive_mode() {
    println!("🤖 Interactive AI Filter Testing Mode");
    println!("Type text to test, 'quit' to exit, 'help' for commands");
    println!("{}", "─".repeat(60));

    loop {
        print!("ai-filter> ");
        use std::io::{self, Write};
        io::stdout().flush().unwrap();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => {
                let input = input.trim();

                if input.is_empty() {
                    continue;
                }

                match input {
                    "quit" | "exit" | "q" => {
                        println!("Goodbye!");
                        break;
                    }
                    "help" | "h" => {
                        show_help();
                    }
                    "test-cases" | "tc" => {
                        run_test_cases().await;
                    }
                    _ => {
                        test_single_text(input).await;
                        println!();
                    }
                }
            }
            Err(error) => {
                println!("Error reading input: {}", error);
                break;
            }
        }
    }
}

async fn batch_mode(filename: &str) {
    use std::fs;
    use std::path::Path;

    if !Path::new(filename).exists() {
        println!("❌ File not found: {}", filename);
        std::process::exit(1);
    }

    let content = match fs::read_to_string(filename) {
        Ok(content) => content,
        Err(e) => {
            println!("❌ Error reading file: {}", e);
            std::process::exit(1);
        }
    };

    println!("🤖 AI Batch Testing: {}", filename);
    println!("{}", "─".repeat(80));

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        println!("\n📝 Line {}: \"{}\"", line_num + 1, line);
        test_single_text(line).await;
    }
}

fn show_help() {
    println!("Commands:");
    println!("  <text>       Test AI filter on the given text");
    println!("  test-cases   Run built-in test cases");
    println!("  help         Show this help");
    println!("  quit         Exit");
}

async fn run_test_cases() {
    println!("🤖 Running AI filter test cases...");
    println!("{}", "─".repeat(60));

    let test_cases = vec![
        // Should be ACCEPTED
        ("Yes", "should accept valid word"),
        ("open file", "should accept command"),
        ("git status", "should accept command"),
        ("How do I configure React?", "should accept technical question"),
        ("set variable to five", "should accept instruction"),

        // Should be REJECTED
        ("Zaaaayyyyyyy", "should reject gibberish"),
        ("stubbornin費이 visit. distortedalgorithm", "should reject mixed gibberish"),
        ("Subtitles by the Amara.org community", "should reject video artifact"),
        ("thank you for watching", "should reject video ending"),
        ("mmm", "should reject sound"),
        ("uh", "should reject filler"),
    ];

    for (text, description) in test_cases {
        println!("\n🔍 {} - {}", description, text);
        test_single_text(text).await;
    }
}