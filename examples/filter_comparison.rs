use std::env;
use always::always::filter;
use always::always::smart_filter;
use always::always::AlwaysConfig;

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        println!("Usage: {} <text_to_test>", args[0]);
        println!("       {} --comprehensive  (run comprehensive comparison)", args[0]);
        std::process::exit(1);
    }

    if args[1] == "--comprehensive" {
        run_comprehensive_comparison().await;
    } else {
        let text = args[1..].join(" ");
        compare_filters(&text).await;
    }
}

async fn compare_filters(text: &str) {
    let config = AlwaysConfig::default();

    println!("🆚 Filter Comparison Test");
    println!("Testing: \"{}\"", text);
    println!("{}", "═".repeat(80));

    // Test old rule-based approach
    let old_result = filter::should_accept_with_reason(text, &config);
    let old_accepted = matches!(old_result, filter::FilterReason::None);

    println!("🔧 RULE-BASED FILTER:");
    println!("   Result: {}", if old_accepted { "✅ ACCEPTED" } else { "❌ REJECTED" });
    println!("   Reason: {}", old_result.to_log_string());
    println!();

    // Test new AI approach (simple fallback)
    let (ai_accepted, ai_reason, ai_corrected) = smart_filter::should_accept_with_simple_ai(text, &config);

    println!("🤖 AI-POWERED FILTER:");
    println!("   Result: {}", if ai_accepted { "✅ ACCEPTED" } else { "❌ REJECTED" });
    println!("   Reason: {}", ai_reason);
    if let Some(corrected) = ai_corrected {
        println!("   Corrected: \"{}\"", corrected);
    }
    println!();

    // Compare results
    if old_accepted == ai_accepted {
        println!("✅ AGREEMENT: Both filters agree on {}",
                 if old_accepted { "ACCEPT" } else { "REJECT" });
    } else {
        println!("⚠️  DISAGREEMENT:");
        println!("   Rule-based: {}", if old_accepted { "ACCEPT" } else { "REJECT" });
        println!("   AI-powered: {}", if ai_accepted { "ACCEPT" } else { "REJECT" });
    }
}

async fn run_comprehensive_comparison() {
    println!("🆚 Comprehensive Filter Comparison");
    println!("{}", "═".repeat(80));

    let test_cases = vec![
        // Valid cases (should be ACCEPTED)
        ("Yes", "Valid single word"),
        ("No", "Valid single word"),
        ("open file", "Simple command"),
        ("git status", "Git command"),
        ("create a new React component", "Programming task"),
        ("How do I configure TypeScript in Next.js?", "Technical question"),
        ("I need to finish this project by tomorrow", "Natural conversation"),
        ("API", "Technical acronym"),
        ("React.js", "Framework name"),
        ("Considering I want to do everything I can to keep Web Speech API, do you think it will be possible to use official and pay or maybe actually just use voice activation detection to start the session at this moment?", "Long complex sentence"),

        // Invalid cases (should be REJECTED)
        ("uh", "Meaningless sound"),
        ("mmm", "Meaningless sound"),
        ("thank you", "Conversational filler"),
        ("good morning", "Greeting"),
        ("Zaaaayyyyyyy", "Gibberish"),
        ("asdfkjhlkj sdflkjsdf lkjsdflkj", "Keyboard mashing"),
        ("qwerty uiop asdf ghjkl zxcv", "Keyboard layout"),
        ("stubbornin費이 visit distortedalgorithm", "Mixed language gibberish"),
        ("Thank you for watching this video", "Video artifact"),
        ("Subtitles by the Amara.org community", "Video watermark"),
    ];

    let mut agreements = 0;
    let mut rule_wins = 0;  // Cases where rule-based seems more correct
    let mut ai_wins = 0;    // Cases where AI seems more correct
    let mut total = test_cases.len();

    for (text, description) in test_cases {
        println!("\n📝 Testing: {} - \"{}\"", description, text);
        println!("{}", "-".repeat(60));

        let config = AlwaysConfig::default();

        // Rule-based
        let old_result = filter::should_accept_with_reason(text, &config);
        let old_accepted = matches!(old_result, filter::FilterReason::None);

        // AI-powered (simple)
        let (ai_accepted, ai_reason, ai_corrected) = smart_filter::should_accept_with_simple_ai(text, &config);

        println!("🔧 Rule-based: {} - {}",
                 if old_accepted { "✅ ACCEPT" } else { "❌ REJECT" },
                 old_result.to_log_string());

        println!("🤖 AI-powered:  {} - {}",
                 if ai_accepted { "✅ ACCEPT" } else { "❌ REJECT" },
                 ai_reason);

        if let Some(corrected) = ai_corrected {
            println!("✏️  Correction:  \"{}\"", corrected);
        }

        if old_accepted == ai_accepted {
            agreements += 1;
            println!("✅ Agreement: Both {}", if old_accepted { "ACCEPT" } else { "REJECT" });
        } else {
            println!("⚠️  Disagreement - Need manual evaluation");

            // For now, just count disagreements
            // In practice, you'd manually evaluate which is better
        }
    }

    println!("\n{}", "═".repeat(80));
    println!("📊 COMPARISON SUMMARY:");
    println!("   Total test cases: {}", total);
    println!("   Agreements: {} ({:.1}%)", agreements, agreements as f32 / total as f32 * 100.0);
    println!("   Disagreements: {} ({:.1}%)", total - agreements, (total - agreements) as f32 / total as f32 * 100.0);

    if std::env::var("ANTHROPIC_API_KEY").is_err() {
        println!("\n⚠️  Note: This test used simple fallback AI rules.");
        println!("   Set ANTHROPIC_API_KEY to test full AI capabilities with Claude API.");
        println!("   The AI approach would likely perform much better with real AI evaluation.");
    }

    println!("\n💡 RECOMMENDATION:");
    if agreements as f32 / total as f32 > 0.8 {
        println!("   Good agreement between approaches. AI approach adds text correction capabilities.");
    } else {
        println!("   Significant differences found. Manual review recommended for production use.");
    }

    println!("   The AI approach offers:");
    println!("   ✅ Intelligent text correction (fix STT errors)");
    println!("   ✅ Contextual understanding (not just pattern matching)");
    println!("   ✅ Simpler maintenance (no complex regex rules)");
    println!("   ✅ Confidence scoring for better UX");
}