fn main() {
    let parent_model = "claude-opus-4-20250514";
    let parts: Vec<&str> = parent_model.rsplit('-').collect();
    println!("rsplit result: {:?}", parts);
    println!("First element (rsplit().next()): {}", parts[0]);
}
