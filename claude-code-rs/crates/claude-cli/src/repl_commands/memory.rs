//! /memory command handler.

/// Handle /memory subcommands.
pub(crate) fn handle_memory_command(sub: &str, cwd: &std::path::Path) {
    let parts: Vec<&str> = sub.splitn(2, ' ').collect();
    match parts.first().copied().unwrap_or("") {
        "" | "list" => {
            let files = claude_core::memory::list_memory_files(cwd);
            if files.is_empty() {
                println!("No memory files found.");
                println!("Create .md files in ~/.claude/memory/ or .claude/memory/ to use memory.");
            } else {
                println!("Memory files ({}):", files.len());
                for f in &files {
                    let type_tag = f.memory_type.as_ref()
                        .map(|t| format!("[{}] ", t.as_str()))
                        .unwrap_or_default();
                    let desc = f.description.as_deref().unwrap_or("");
                    println!("  {}{:<40} {}", type_tag, f.filename, desc);
                }
            }
        }
        "open" => {
            let rel_path = parts.get(1).copied().unwrap_or("").trim();
            if rel_path.is_empty() {
                println!("Usage: /memory open <filename>");
                return;
            }
            // Validate: reject path traversal attempts
            if rel_path.contains("..") || rel_path.starts_with('/') || rel_path.starts_with('\\') || rel_path.contains(':') {
                println!("Invalid filename: must be a simple name without path separators or '..'");
                return;
            }
            // Try to find the file in memory dirs
            let mem_dirs = claude_core::memory::memory_dirs(cwd);
            let mut found = false;
            for dir in &mem_dirs {
                let p = dir.join(rel_path);
                // Verify resolved path stays inside the memory directory
                if let Ok(canonical) = p.canonicalize() {
                    if !canonical.starts_with(dir) {
                        continue;
                    }
                }
                if p.exists() {
                    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "notepad".into());
                    let _ = std::process::Command::new(&editor).arg(&p).status();
                    found = true;
                    break;
                }
            }
            if !found {
                println!("Memory file '{}' not found in any memory directory.", rel_path);
            }
        }
        "add" => {
            let content = parts.get(1).copied().unwrap_or("").trim();
            if content.is_empty() {
                println!("Usage: /memory add <content>");
                return;
            }
            // Write to project memory dir
            let mem_dir = cwd.join(".claude").join("memory");
            if let Err(e) = std::fs::create_dir_all(&mem_dir) {
                eprintln!("\x1b[31mFailed to create memory dir: {}\x1b[0m", e);
                return;
            }
            // Auto-generate filename from content prefix
            let slug: String = content.chars()
                .take(40)
                .filter(|c| c.is_alphanumeric() || *c == ' ')
                .collect::<String>()
                .trim()
                .replace(' ', "_")
                .to_lowercase();
            let filename = format!("{}.md", if slug.is_empty() { "note" } else { &slug });
            let path = mem_dir.join(&filename);
            match std::fs::write(&path, content) {
                Ok(()) => println!("\x1b[32m✓ Memory saved to {}\x1b[0m", path.display()),
                Err(e) => eprintln!("\x1b[31mFailed to write memory: {}\x1b[0m", e),
            }
        }
        other => {
            println!("Unknown memory subcommand: '{}'. Use list, open <file>, or add <content>.", other);
        }
    }
}
