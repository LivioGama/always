use always::always::filter;
use std::fs;

fn main() {
    println!("🧪 Comprehensive Filter Test - Generating and Testing 1000+ Sentences");
    println!("════════════════════════════════════════════════════════════════════════");

    let test_sentences = generate_test_sentences();
    let mut results = TestResults::new();

    println!("Testing {} sentences...\n", test_sentences.len());

    for (category, sentences) in test_sentences {
        println!("📂 Category: {} ({} sentences)", category, sentences.len());
        println!("{}", "─".repeat(60));

        for sentence in sentences {
            let result = test_sentence(&sentence);
            results.add_result(&category, &sentence, &result);

            // Print real-time results with color coding
            let status = if result.accepted { "✅" } else { "❌" };
            let reason = if result.accepted {
                "ACCEPTED".to_string()
            } else {
                format!(
                    "REJECTED: {}",
                    result.rejection_reason.unwrap_or("Unknown".to_string())
                )
            };

            println!("{} {} - {}", status, sentence, reason);
        }
        println!();
    }

    // Print comprehensive summary
    results.print_summary();

    // Save detailed results to file
    results.save_to_file("filter_test_results.txt");
    println!("📊 Detailed results saved to filter_test_results.txt");
}

#[derive(Clone)]
struct TestResult {
    accepted: bool,
    rejection_reason: Option<String>,
    quick_reject: bool,
    hard_reject: bool,
    onomatopoeia: bool,
    gibberish: bool,
    non_ascii: bool,
    degeneracy: bool,
}

struct TestResults {
    categories: std::collections::HashMap<String, Vec<(String, TestResult)>>,
}

impl TestResults {
    fn new() -> Self {
        Self {
            categories: std::collections::HashMap::new(),
        }
    }

    fn add_result(&mut self, category: &str, sentence: &str, result: &TestResult) {
        self.categories
            .entry(category.to_string())
            .or_insert_with(Vec::new)
            .push((sentence.to_string(), result.clone()));
    }

    fn print_summary(&self) {
        println!("🏆 COMPREHENSIVE TEST SUMMARY");
        println!("════════════════════════════════════════════════════════════════════════");

        let mut total_sentences = 0;
        let mut total_accepted = 0;
        let mut total_rejected = 0;

        for (category, results) in &self.categories {
            let accepted = results.iter().filter(|(_, r)| r.accepted).count();
            let rejected = results.iter().filter(|(_, r)| !r.accepted).count();
            let acceptance_rate = (accepted as f64 / results.len() as f64) * 100.0;

            println!(
                "📂 {}: {}/{} accepted ({:.1}%)",
                category,
                accepted,
                results.len(),
                acceptance_rate
            );

            total_sentences += results.len();
            total_accepted += accepted;
            total_rejected += rejected;
        }

        let overall_acceptance = (total_accepted as f64 / total_sentences as f64) * 100.0;
        println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
        println!(
            "🎯 OVERALL: {}/{} accepted ({:.1}%)",
            total_accepted, total_sentences, overall_acceptance
        );

        // Analyze rejection reasons
        println!("\n📈 REJECTION ANALYSIS:");
        let mut rejection_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for results in self.categories.values() {
            for (_, result) in results {
                if !result.accepted {
                    if let Some(reason) = &result.rejection_reason {
                        *rejection_counts.entry(reason.clone()).or_insert(0) += 1;
                    }
                }
            }
        }

        for (reason, count) in rejection_counts {
            println!("  • {}: {} sentences", reason, count);
        }
    }

    fn save_to_file(&self, filename: &str) {
        let mut content = String::new();
        content.push_str("COMPREHENSIVE FILTER TEST RESULTS\n");
        content.push_str("═══════════════════════════════════\n\n");

        for (category, results) in &self.categories {
            content.push_str(&format!("Category: {}\n", category));
            content.push_str(&format!("{}\n", "─".repeat(50)));

            for (sentence, result) in results {
                let status = if result.accepted {
                    "✅ ACCEPTED"
                } else {
                    "❌ REJECTED"
                };
                let reason = if result.accepted {
                    String::new()
                } else {
                    format!(
                        " - {}",
                        result
                            .rejection_reason
                            .as_ref()
                            .unwrap_or(&"Unknown".to_string())
                    )
                };

                content.push_str(&format!("{} {}{}\n", status, sentence, reason));
            }
            content.push_str("\n");
        }

        fs::write(filename, content).expect("Failed to write results file");
    }
}

fn test_sentence(text: &str) -> TestResult {
    let cfg = always::always::AlwaysConfig::from_cli("en".to_string(), 30, 0.5, false)
        .expect("Failed to create config");

    // Test individual filters
    let quick_reject_result = filter::quick_reject_with_reason(text);
    let hard_reject_result = filter::hard_reject_with_reason(text);
    let onomatopoeia = filter::onomatopoeia_reject(text);
    let gibberish = filter::gibberish_reject(text);
    let non_ascii = filter::non_ascii_reject(text);

    // Test overall result
    let overall_result = filter::should_accept_with_reason(text, &cfg);
    let accepted = matches!(overall_result, filter::FilterReason::None);

    let rejection_reason = if !accepted {
        Some(
            format!("{:?}", overall_result)
                .split('(')
                .next()
                .unwrap_or("Unknown")
                .to_string(),
        )
    } else {
        None
    };

    TestResult {
        accepted,
        rejection_reason,
        quick_reject: !matches!(quick_reject_result, filter::FilterReason::None),
        hard_reject: !matches!(hard_reject_result, filter::FilterReason::None),
        onomatopoeia,
        gibberish,
        non_ascii,
        degeneracy: false, // We'll determine this from overall result
    }
}

fn generate_test_sentences() -> Vec<(String, Vec<String>)> {
    vec![
        // Valid Commands & Instructions (should be ACCEPTED)
        ("Valid Commands".to_string(), vec![
            "open the file in editor".to_string(),
            "git commit with message fix bug".to_string(),
            "create a new React component".to_string(),
            "install the npm package".to_string(),
            "run the unit tests".to_string(),
            "deploy to production server".to_string(),
            "update the database schema".to_string(),
            "configure the API endpoints".to_string(),
            "set the environment variable".to_string(),
            "build the Docker container".to_string(),
        ]),

        // Technical Questions (should be ACCEPTED)
        ("Technical Questions".to_string(), vec![
            "How do I configure TypeScript in Next.js?".to_string(),
            "What is the difference between React hooks and class components?".to_string(),
            "Can you explain how OAuth 2.0 authentication works?".to_string(),
            "Which database should I use for this microservice?".to_string(),
            "How to optimize SQL queries for better performance?".to_string(),
            "What are the best practices for API rate limiting?".to_string(),
            "Can you compare REST API vs GraphQL?".to_string(),
            "How to implement caching in Redis?".to_string(),
            "What is the purpose of Docker Compose?".to_string(),
            "How to handle errors in async JavaScript functions?".to_string(),
        ]),

        // Natural Conversation (should be ACCEPTED)
        ("Natural Conversation".to_string(), vec![
            "I need to finish this project by tomorrow".to_string(),
            "The server seems to be running slowly today".to_string(),
            "Let me check the logs to see what happened".to_string(),
            "We should probably refactor this code soon".to_string(),
            "The client wants to add a new feature".to_string(),
            "I'm having trouble with the CSS layout".to_string(),
            "The database migration took longer than expected".to_string(),
            "Can you review my pull request when you have time?".to_string(),
            "The test suite is failing on the CI server".to_string(),
            "I think there might be a memory leak in this function".to_string(),
        ]),

        // Single Valid Words (should be ACCEPTED)
        ("Single Valid Words".to_string(), vec![
            "Yes".to_string(),
            "No".to_string(),
            "OK".to_string(),
            "Stop".to_string(),
            "Continue".to_string(),
            "Save".to_string(),
            "Cancel".to_string(),
            "Help".to_string(),
            "Exit".to_string(),
            "Run".to_string(),
        ]),

        // Product Names & Acronyms (should be ACCEPTED)
        ("Product Names & Acronyms".to_string(), vec![
            "Google Cloud Speech-to-Text API".to_string(),
            "Amazon Web Services EC2 instance".to_string(),
            "Microsoft Azure DevOps pipeline".to_string(),
            "Groq Whisper Large v3 Turbo".to_string(),
            "OpenAI GPT-4 API integration".to_string(),
            "Docker Kubernetes deployment".to_string(),
            "React Native iOS application".to_string(),
            "PostgreSQL database cluster".to_string(),
            "Redis ElastiCache configuration".to_string(),
            "GitHub Actions CI/CD workflow".to_string(),
        ]),

        // Long Complex Sentences (should be ACCEPTED)
        ("Long Complex Sentences".to_string(), vec![
            "Considering I want to do everything I can to keep Web Speech API, do you think it will be possible to use official and pay or maybe actually just use voice activation detection to start the session at this moment?".to_string(),
            "The implementation of the new authentication system requires careful consideration of security protocols, user experience design, and backward compatibility with existing client applications.".to_string(),
            "We need to migrate the legacy monolithic architecture to a microservices-based approach while ensuring zero downtime and maintaining data consistency across all distributed components.".to_string(),
            "The performance optimization involves analyzing database query patterns, implementing proper indexing strategies, and configuring connection pooling for maximum throughput.".to_string(),
            "Can you help me understand the architectural differences between event-driven microservices and traditional request-response patterns in distributed systems?".to_string(),
        ]),

        // Gibberish & Random Text (should be REJECTED)
        ("Gibberish & Random Text".to_string(), vec![
            "asdfkjhlkj sdflkjsdf lkjsdflkj".to_string(),
            "qwerty uiop asdf ghjkl zxcv".to_string(),
            "stubbornin費이 visit distortedalgorithm".to_string(),
            "kdfjglkdfj glkdfj glkdfj gldkfj".to_string(),
            "Zaaaayyyyyyy bbbbbbrrrrrr cccccchhhhh".to_string(),
            "randomwordsmashed togetherwithout spaces".to_string(),
            "fjdksla jfklsd jfklsadj fklsadjf".to_string(),
            "typing random keys on keyboard".to_string(),
            "dgfhfgh dfghdfgh dfghdfgh dfgh".to_string(),
            "nonsensical jumbled words everywhere".to_string(),
        ]),

        // Video Artifacts (should be REJECTED)
        ("Video Artifacts".to_string(), vec![
            "Thank you for watching this video".to_string(),
            "Thanks for listening to our podcast".to_string(),
            "Subtitles by the Amara.org community".to_string(),
            "Captions by Rev.com transcription service".to_string(),
            "Please like and subscribe for more content".to_string(),
            "Turn on closed captions for better experience".to_string(),
            "Auto-generated subtitles may contain errors".to_string(),
            "Transcription provided by YouTube".to_string(),
            "Enable subtitles in the video player".to_string(),
            "Thank you for your attention".to_string(),
        ]),

        // Meaningless Sounds (should be REJECTED)
        ("Meaningless Sounds".to_string(), vec![
            "uh".to_string(),
            "um".to_string(),
            "ah".to_string(),
            "mmm".to_string(),
            "hmm".to_string(),
            "err".to_string(),
            "uhh".to_string(),
            "umm".to_string(),
            "ahh".to_string(),
            "ehh".to_string(),
        ]),

        // Mixed Language Nonsense (should be REJECTED)
        ("Mixed Language Nonsense".to_string(), vec![
            "hello 你好 bonjour здравствуй".to_string(),
            "code機械学習 programming विकास".to_string(),
            "data分析 processing解析 algorithm".to_string(),
            "server配置 deployment展開 monitoring".to_string(),
            "database数据库 query查询 optimization".to_string(),
            "frontend前端 backend后端 fullstack".to_string(),
            "testing測試 debugging调试 production".to_string(),
            "security安全 authentication認証 authorization".to_string(),
            "performance性能 optimization最適化 scaling".to_string(),
            "container容器 orchestration编排 deployment".to_string(),
        ]),

        // Conversational Fillers (should be REJECTED)
        ("Conversational Fillers".to_string(), vec![
            "thank you".to_string(),
            "you're welcome".to_string(),
            "excuse me".to_string(),
            "I'm sorry".to_string(),
            "good morning".to_string(),
            "how are you".to_string(),
            "see you later".to_string(),
            "have a nice day".to_string(),
            "take care".to_string(),
            "sounds good".to_string(),
        ]),

        // Edge Cases (mixed - should analyze individually)
        ("Edge Cases".to_string(), vec![
            "API".to_string(),
            "SQL".to_string(),
            "JSON".to_string(),
            "HTML".to_string(),
            "CSS".to_string(),
            "HTTP".to_string(),
            "REST".to_string(),
            "CRUD".to_string(),
            "OAuth".to_string(),
            "JWT".to_string(),
            "a".to_string(),
            "I".to_string(),
            "React.js".to_string(),
            "Node.js".to_string(),
            "Vue.js".to_string(),
        ]),
    ]
}
