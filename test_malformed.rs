#[cfg(test)]
mod test_malformed_domains {
    use crate::always::ai_filter::AiFilter;

    #[test]
    fn test_fallback_malformed_domain() {
        let filter = AiFilter::new("dummy".to_string(), None);
        let result = filter.evaluate_fallback("expenseÁê•-tee.com");

        println!("Text: expenseÁê•-tee.com");
        println!("Should accept: {}", result.should_accept);
        println!("Confidence: {:.1}%", result.confidence_score * 100.0);
        println!("Reason: {}", result.reason);
        println!("Category: {:?}", result.filter_category);

        // The fallback should catch this as malformed domain-like text
        assert!(!result.should_accept, "Malformed domain should be rejected");
    }
}