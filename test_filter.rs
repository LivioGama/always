use always::filter::hard_reject_with_reason;

fn main() {
    let result = hard_reject_with_reason("Thank you for watching.");
    println!("Result: {:?}", result);
    println!("Should reject: {}", !matches!(result, always::filter::FilterReason::None));
}
