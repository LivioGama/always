use always::always::filter;
use std::env;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("Usage: {} <text_to_test>", args[0]);
        println!("       {} --interactive  (for interactive mode)", args[0]);
        println!("       {} --batch <file>  (test lines from file)", args[0]);
        std::process::exit(1);
    }

    if args[1] == "--interactive" {
        interactive_mode();
    } else if args[1] == "--batch" && args.len() > 2 {
        batch_mode(&args[2]);
    } else {
        let text = args[1..].join(" ");
        test_single_text(&text);
    }
}

fn test_single_text(text: &str) {
    println!("Testing: \"{}\"", text);
    println!("{}", "─".repeat(50));

    // Test each filter individually
    let quick_result = filter::quick_reject_with_reason(text);
    println!("quick_reject: {:?}", quick_result);

    let hard_result = filter::hard_reject_with_reason(text);
    println!("hard_reject: {:?}", hard_result);

    let onomatopoeia = filter::onomatopoeia_reject(text);
    println!("onomatopoeia_reject: {}", onomatopoeia);

    let gibberish = filter::gibberish_reject(text);
    println!("gibberish_reject: {}", gibberish);

    let non_ascii = filter::non_ascii_reject(text);
    println!("non_ascii_reject: {}", non_ascii);

    // Test overall filter decision
    let cfg = always::always::AlwaysConfig::default();
    let overall_result = filter::should_accept_with_reason(text, &cfg);
    let should_accept = matches!(overall_result, filter::FilterReason::None);

    println!("{}", "─".repeat(50));
    if should_accept {
        println!("✅ ACCEPTED: Text would be pasted");
    } else {
        println!(
            "❌ REJECTED: {} - {}",
            format!("{:?}", overall_result)
                .split('(')
                .next()
                .unwrap_or("Unknown"),
            overall_result.to_log_string()
        );
    }
}

fn interactive_mode() {
    println!("🧪 Interactive Filter Testing Mode");
    println!("Type text to test, 'quit' to exit, 'help' for commands");
    println!("{}", "─".repeat(50));

    loop {
        print!("filter> ");
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
                        run_test_cases();
                    }
                    _ => {
                        test_single_text(input);
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

fn batch_mode(filename: &str) {
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

    println!("🧪 Batch Testing: {}", filename);
    println!("{}", "─".repeat(80));

    for (line_num, line) in content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        println!("\n📝 Line {}: \"{}\"", line_num + 1, line);
        test_single_text(line);
    }
}

fn show_help() {
    println!("Commands:");
    println!("  <text>       Test filter on the given text");
    println!("  test-cases   Run built-in test cases");
    println!("  help         Show this help");
    println!("  quit         Exit");
}

fn run_test_cases() {
    println!("🧪 Running built-in test cases...");
    println!("{}", "─".repeat(50));

    let test_cases = vec![
        // Should be REJECTED
        ("Zaaaayyyyyyy", "should reject repeated chars"),
        (
            "stubbornin費이 visit. distortedalgorithm",
            "should reject mixed gibberish",
        ),
        (
            "Subtitles by the Amara.org community",
            "should reject video artifact",
        ),
        ("thank you for watching", "should reject video ending"),
        ("mmm", "should reject sound"),
        ("uh", "should reject filler"),
        ("okay.", "should reject conversation filler"),
        // Should be ACCEPTED
        ("Yes", "should accept valid short word"),
        ("open file", "should accept command"),
        ("git status", "should accept command"),
        ("create a new function", "should accept longer phrase"),
        ("set variable to five", "should accept instruction"),
    ];

    for (text, description) in test_cases {
        println!("\n🔍 {} - {}", description, text);
        test_single_text(text);
    }
}
