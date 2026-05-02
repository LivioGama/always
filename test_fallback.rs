use always::always::ai_filter::AiFilter;

fn main() {
    let filter = AiFilter::new("dummy".to_string(), None);
    let result = filter.evaluate_fallback("expenseÁê•-tee.com");

    println!("Text: expenseÁê•-tee.com");
    println!("Should accept: {}", result.should_accept);
    println!("Confidence: {:.1}%", result.confidence_score * 100.0);
    println!("Reason: {}", result.reason);
    println!("Category: {:?}", result.filter_category);
}