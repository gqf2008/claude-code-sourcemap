fn main() {
    // Test with date suffix
    let parent_model = "claude-opus-4-20250514";
    let date_suffix = parent_model.rsplit('-').next()
        .filter(|s| s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()))
        .map(|d| format!("-{}", d))
        .unwrap_or_default();
    println!("With date: '{}' -> suffix: '{}'", parent_model, date_suffix);
    
    // Test WITHOUT date suffix - this is the problem case!
    let parent_model2 = "claude-opus-4";
    let date_suffix2 = parent_model2.rsplit('-').next()
        .filter(|s| s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()))
        .map(|d| format!("-{}", d))
        .unwrap_or_default();
    println!("Without date: '{}' -> suffix: '{}'", parent_model2, date_suffix2);
    
    // What gets returned?
    let result = format!("claude-sonnet-4{}", date_suffix2);
    println!("Result for no-date case: '{}'", result);
    
    // Test with hyphenated model name (e.g., haiku-3.5-20250514)
    let parent_model3 = "claude-haiku-3.5-20250514";
    let date_suffix3 = parent_model3.rsplit('-').next()
        .filter(|s| s.len() == 8 && s.chars().all(|c| c.is_ascii_digit()))
        .map(|d| format!("-{}", d))
        .unwrap_or_default();
    println!("Haiku with date: '{}' -> suffix: '{}'", parent_model3, date_suffix3);
}
