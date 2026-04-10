use std::sync::Arc;
use std::time::Duration;

use claude_agent::engine::QueryEngine;
use claude_agent::plugin::PluginLoader;
use claude_bus::bus::ClientHandle;
use claude_bus::events::AgentRequest;
use claude_core::file_watcher::ConfigWatcher;

use crate::commands::{CommandResult, SlashCommand};
use crate::config;
use crate::input::{InputReader, InputResult, history_file_path};
use crate::output::{print_stream, spawn_esc_listener, OutputRenderer};
use crate::repl_commands::*;
use crate::theme;

/// Timeout for command-response notification loops (e.g. ClearHistory, SetModel).
const CMD_NOTIFICATION_TIMEOUT: Duration = Duration::from_secs(30);

/// Receive next notification with timeout. Returns None on timeout or channel close.
async fn recv_with_timeout(
    client: &mut ClientHandle,
    timeout: Duration,
) -> Option<claude_bus::events::AgentNotification> {
    match tokio::time::timeout(timeout, client.recv_notification()).await {
        Ok(Some(n)) => Some(n),
        Ok(None) | Err(_) => None,
    }
}

pub async fn run(
    engine: Arc<QueryEngine>,
    mut client: Option<ClientHandle>,
    cwd: std::path::PathBuf,
) -> anyhow::Result<()> {
    let current_model = engine.state().read().await.model.clone();
    let display = claude_core::model::display_name_any(&current_model);
    let border = theme::c_prompt();
    println!("{border}╭─────────────────────────────────╮\x1b[0m");
    println!("{border}│      Claude Code (Rust)         │\x1b[0m");
    println!("{border}│  Model: {:<23} │\x1b[0m", display);
    println!("{border}│  cwd: {:<25} │\x1b[0m", truncate_path(&cwd, 25));
    println!("{border}│  Type /help for commands        │\x1b[0m");
    println!("{border}│  Shift/Alt+Enter for newline    │\x1b[0m");
    println!("{border}╰─────────────────────────────────╯\x1b[0m\n");

    // Lazy-loaded and cached — first call scans disk, subsequent calls O(1)
    let startup_skills = claude_core::skills::get_skills(&cwd);
    if !startup_skills.is_empty() {
        let names: Vec<&str> = startup_skills.iter().map(|s| s.name.as_str()).collect();
        println!("{}Skills loaded: {}\x1b[0m\n", theme::c_warn(), names.join(", "));
    }

    let mut rl = InputReader::new();

    // Load persistent history
    let hist_path = history_file_path();
    if let Some(ref path) = hist_path {
        rl.load_history(path);
    }

    // Start real-time config file watcher (CLAUDE.md + settings.json)
    let mut config_watcher = ConfigWatcher::start(&cwd).ok();

    // Session start time for /stats display
    let session_start = std::time::Instant::now();

    // Periodic session checkpoint counter (save every N turns to prevent data loss)
    const CHECKPOINT_INTERVAL: u32 = 5;
    let mut turns_since_save: u32 = 0;

    // Images queued via /image command (merged with @-references on next submit)
    let mut pending_images: Vec<claude_core::message::ContentBlock> = Vec::new();

    loop {
        // Context usage warning before prompt
        if let Some(pct) = engine.context_usage_percent().await {
            if pct >= 95 {
                eprintln!("{}⚠ Context {pct}% full — compaction imminent\x1b[0m", theme::c_err());
            } else if pct >= 80 {
                eprintln!("{}⚠ Context {pct}% full\x1b[0m", theme::c_warn());
            }
        }

        let readline = rl.readline("> ");
        match readline {
            Ok(InputResult::Line(line)) => {
                let trimmed = line.trim();
                if trimmed.is_empty() { continue; }

                // Parse slash commands BEFORE multiline expansion
                if trimmed.starts_with('/') {
                    rl.add_history(trimmed);
                    // Re-fetch skills each time (cached; refreshed by /reload-context)
                    let skills = claude_core::skills::get_skills(&cwd);
                    if let Some(cmd) = SlashCommand::parse(trimmed, &skills) {
                        let loader = PluginLoader::discover(&cwd);
                        // Resolve Unknown commands: check if they match a plugin command
                        let cmd = if let SlashCommand::Unknown(ref name) = cmd {
                            let found = loader.all_commands().into_iter()
                                .find(|(_, c)| c.name == *name);
                            if let Some((plugin, pcmd)) = found {
                                if let Some(prompt) = PluginLoader::command_prompt(plugin, pcmd) {
                                    SlashCommand::RunPluginCommand {
                                        name: name.clone(),
                                        prompt,
                                    }
                                } else {
                                    eprintln!("{}Plugin command /{} has no prompt file\x1b[0m", theme::c_warn(), name);
                                    cmd
                                }
                            } else {
                                cmd
                            }
                        } else {
                            cmd
                        };
                        // Build plugin command list for help display
                        let plugin_cmds: Vec<crate::commands::PluginCommandEntry> =
                            loader.all_commands().into_iter()
                                .map(|(p, c)| crate::commands::PluginCommandEntry {
                                    plugin_name: p.manifest.name.clone(),
                                    command_name: c.name.clone(),
                                })
                                .collect();
                        match cmd.execute(&skills, &plugin_cmds) {
                            CommandResult::Print(text) => println!("{}", text),
                            CommandResult::Exit => { println!("Goodbye!"); break; }
                            CommandResult::ClearHistory => {
                                if let Some(ref mut c) = client {
                                    let _ = c.send_request(AgentRequest::ClearHistory);
                                    // Wait for HistoryCleared notification
                                    while let Some(n) = recv_with_timeout(c, CMD_NOTIFICATION_TIMEOUT).await {
                                        if matches!(n, claude_bus::events::AgentNotification::HistoryCleared) {
                                            println!("Conversation history cleared.");
                                            break;
                                        }
                                    }
                                } else {
                                    engine.clear_history().await;
                                    println!("Conversation history cleared.");
                                }
                            }
                            CommandResult::SetModel(input) => {
                                if let Some(ref mut c) = client {
                                    let _ = c.send_request(AgentRequest::SetModel { model: input.clone() });
                                    // Wait for ModelChanged notification
                                    while let Some(n) = recv_with_timeout(c, CMD_NOTIFICATION_TIMEOUT).await {
                                        if let claude_bus::events::AgentNotification::ModelChanged { model, display_name } = n {
                                            println!("Model set to: {} ({})", display_name, model);
                                            // Persist to user settings
                                            if let Err(e) = claude_core::config::Settings::update_field(
                                                claude_core::config::SettingsSource::User,
                                                &cwd,
                                                |s| { s.model = Some(model.clone()); },
                                            ) {
                                                eprintln!("{}Note: Could not persist model choice: {}\x1b[0m", theme::c_warn(), e);
                                            }
                                            break;
                                        }
                                    }
                                } else {
                                    let resolved = claude_core::model::resolve_model_string(&input);
                                    let state = engine.state();
                                    let mut s = state.write().await;
                                    s.model = resolved.clone();
                                    let display = claude_core::model::display_name_any(&resolved);
                                    println!("Model set to: {} ({})", display, resolved);

                                    // Persist to user settings
                                    if let Err(e) = claude_core::config::Settings::update_field(
                                        claude_core::config::SettingsSource::User,
                                        &cwd,
                                        |s| { s.model = Some(resolved.clone()); },
                                    ) {
                                        eprintln!("{}Note: Could not persist model choice: {}\x1b[0m", theme::c_warn(), e);
                                    }
                                }
                            }
                            CommandResult::ShowCost { window } => {
                                let state = engine.state();
                                let s = state.read().await;
                                let cost_window = claude_agent::cost::CostWindow::parse(&window);
                                let summary = engine.cost_tracker().format_summary_window(
                                    s.total_input_tokens,
                                    s.total_output_tokens,
                                    s.turn_count,
                                    cost_window,
                                );
                                println!("{}", summary);
                            }
                            CommandResult::Compact { instructions } => {
                                if let Some(ref mut c) = client {
                                    println!("{}Compacting conversation…\x1b[0m", theme::c_warn());
                                    let _ = c.send_request(AgentRequest::Compact { instructions });
                                    // Wait for CompactComplete or Error notification
                                    while let Some(n) = recv_with_timeout(c, CMD_NOTIFICATION_TIMEOUT).await {
                                        match n {
                                            claude_bus::events::AgentNotification::CompactComplete { summary_len } => {
                                                println!("{}✓ Compacted ({} chars).\x1b[0m", theme::c_ok(), summary_len);
                                                break;
                                            }
                                            claude_bus::events::AgentNotification::Error { message, .. } => {
                                                eprintln!("{}Compact failed: {}\x1b[0m", theme::c_err(), message);
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                } else {
                                    println!("{}Compacting conversation…\x1b[0m", theme::c_warn());
                                    match engine.compact("manual", instructions.as_deref()).await {
                                        Ok(summary) => {
                                            println!("{}✓ Compacted.\x1b[0m", theme::c_ok());
                                            let preview: String = summary.lines().take(5).collect::<Vec<_>>().join("\n");
                                            println!("\x1b[2m{}\x1b[0m", preview);
                                        }
                                        Err(e) => eprintln!("{}Compact failed: {}\x1b[0m", theme::c_err(), e),
                                    }
                                }
                            }
                            CommandResult::Memory { sub } => {
                                handle_memory_command(&sub, &cwd);
                            }
                            CommandResult::Session { sub } => {
                                handle_session_command(&sub, &engine).await;
                            }
                            CommandResult::Diff => {
                                handle_diff_command(&cwd);
                            }
                            CommandResult::Status => {
                                if let Some(ref mut c) = client {
                                    let _ = c.send_request(AgentRequest::GetStatus);
                                    while let Some(n) = recv_with_timeout(c, CMD_NOTIFICATION_TIMEOUT).await {
                                        if let claude_bus::events::AgentNotification::SessionStatus {
                                            model, total_turns, total_input_tokens,
                                            total_output_tokens, context_usage_pct, ..
                                        } = n {
                                            println!("Model: {}", model);
                                            println!("Turns: {}", total_turns);
                                            println!("Tokens: {} in / {} out", total_input_tokens, total_output_tokens);
                                            println!("Context: {:.0}%", context_usage_pct);
                                            break;
                                        }
                                    }
                                } else {
                                    handle_status_command(&engine, &cwd).await;
                                }
                            }
                            CommandResult::Permissions { mode } => {
                                if mode.is_empty() {
                                    let s = engine.state().read().await;
                                    println!("Permission mode: {:?}", s.permission_mode);
                                    println!("  \x1b[2mSet with: /permissions <default|bypass|acceptEdits|plan>\x1b[0m");
                                } else {
                                    let new_mode = config::parse_permission_mode(&mode);
                                    engine.state().write().await.permission_mode = new_mode;
                                    println!("Permission mode: {:?}", new_mode);
                                }
                            }
                            CommandResult::Config => {
                                handle_config_command(&cwd);
                            }
                            CommandResult::Undo => {
                                handle_undo(&engine).await;
                            }
                            CommandResult::Review { prompt } => {
                                handle_review(&engine, &prompt, &cwd).await;
                            }
                            CommandResult::PrComments { pr_number } => {
                                handle_pr_comments(&engine, pr_number, &cwd).await;
                            }
                            CommandResult::Branch { name } => {
                                handle_branch(&engine, &name).await;
                            }
                            CommandResult::Doctor => {
                                handle_doctor(&engine, &cwd).await;
                            }
                            CommandResult::Init => {
                                handle_init(&engine, &cwd).await;
                            }
                            CommandResult::Commit { message } => {
                                handle_commit(&engine, &cwd, &message).await;
                            }
                            CommandResult::CommitPushPr { message } => {
                                handle_commit_push_pr(&engine, &cwd, &message).await;
                            }
                            CommandResult::Pr { prompt } => {
                                handle_pr(&engine, &prompt, &cwd).await;
                            }
                            CommandResult::Bug { prompt } => {
                                handle_bug(&engine, &prompt, &cwd).await;
                            }
                            CommandResult::Search { query } => {
                                handle_search(&engine, &query).await;
                            }
                            CommandResult::History { page } => {
                                handle_history(&engine, page).await;
                            }
                            CommandResult::Retry => {
                                if let Some(prompt) = engine.pop_last_turn().await {
                                    eprintln!("{}[Retrying: {}…]\x1b[0m", theme::c_warn(),
                                        if prompt.len() > 50 { &prompt[..50] } else { &prompt });
                                    let model = { engine.state().read().await.model.clone() };
                                    let stream = engine.submit(&prompt).await;
                                    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker()), Some(&engine.abort_signal())).await {
                                        eprintln!("{}Retry error: {}\x1b[0m", theme::c_err(), e);
                                    }
                                    print_turn_stats(&engine).await;
                                } else {
                                    println!("No previous prompt to retry.");
                                }
                            }
                            CommandResult::Login => {
                                handle_login();
                            }
                            CommandResult::Logout => {
                                handle_logout();
                            }
                            CommandResult::Context => {
                                handle_context(&engine, &cwd).await;
                            }
                            CommandResult::Export { format } => {
                                handle_export(&engine, &cwd, &format).await;
                            }
                            CommandResult::ReloadContext => {
                                handle_reload_context(&engine, &cwd).await;
                            }
                            CommandResult::Mcp { sub } => {
                                handle_mcp_command(&sub, &cwd);
                            }
                            CommandResult::Plugin { sub } => {
                                handle_plugin_command(&sub, &cwd);
                            }
                            CommandResult::RunSkill { name, prompt } => {
                                run_skill(&engine, &skills, &name, &prompt, &rl).await;
                            }
                            CommandResult::RunPluginCommand { name, prompt } => {
                                handle_plugin_run(&engine, &name, &prompt).await;
                            }
                            CommandResult::Agents { sub } => {
                                handle_agents_command(&sub, &cwd, None);
                            }
                            CommandResult::Theme { name } => {
                                handle_theme_command(&name);
                            }
                            CommandResult::Plan { args } => {
                                handle_plan_command(&args, &engine, &cwd).await;
                            }
                            CommandResult::Think { args } => {
                                if let Some(ref mut c) = client {
                                    let mode = if args.is_empty() {
                                        // Toggle: if currently enabled → off, else on
                                        if engine.thinking_config().is_some() { "off".to_string() } else { "on".to_string() }
                                    } else {
                                        args.clone()
                                    };
                                    let _ = c.send_request(AgentRequest::SetThinking { mode });
                                    while let Some(n) = recv_with_timeout(c, CMD_NOTIFICATION_TIMEOUT).await {
                                        if let claude_bus::events::AgentNotification::ThinkingChanged { enabled, budget } = n {
                                            if enabled {
                                                let budget_str = budget.map(|b| format!(" (budget: {})", b)).unwrap_or_default();
                                                println!("{}✓ Extended thinking enabled{}\x1b[0m", theme::c_ok(), budget_str);
                                            } else {
                                                println!("{}✓ Extended thinking disabled\x1b[0m", theme::c_ok());
                                            }
                                            break;
                                        }
                                    }
                                } else {
                                    // Direct mode (no bus)
                                    let mode = if args.is_empty() {
                                        if engine.thinking_config().is_some() { "off" } else { "on" }
                                    } else {
                                        args.as_str()
                                    };
                                    match mode.to_lowercase().as_str() {
                                        "off" | "false" | "0" | "disable" => {
                                            engine.set_thinking(None);
                                            println!("{}✓ Extended thinking disabled\x1b[0m", theme::c_ok());
                                        }
                                        "on" | "true" | "enable" => {
                                            engine.set_thinking(Some(claude_api::types::ThinkingConfig {
                                                thinking_type: "enabled".into(),
                                                budget_tokens: Some(10_000),
                                            }));
                                            println!("{}✓ Extended thinking enabled (budget: 10000)\x1b[0m", theme::c_ok());
                                        }
                                        other => {
                                            if let Ok(budget) = other.parse::<u32>() {
                                                engine.set_thinking(Some(claude_api::types::ThinkingConfig {
                                                    thinking_type: "enabled".into(),
                                                    budget_tokens: Some(budget),
                                                }));
                                                println!("{}✓ Extended thinking enabled (budget: {})\x1b[0m", theme::c_ok(), budget);
                                            } else {
                                                println!("Usage: /think [on|off|<budget>]");
                                            }
                                        }
                                    }
                                }
                            }
                            CommandResult::BreakCache => {
                                if let Some(ref mut c) = client {
                                    let _ = c.send_request(AgentRequest::BreakCache);
                                    while let Some(n) = recv_with_timeout(c, CMD_NOTIFICATION_TIMEOUT).await {
                                        if matches!(n, claude_bus::events::AgentNotification::CacheBreakSet) {
                                            println!("{}✓ Next request will skip prompt cache\x1b[0m", theme::c_ok());
                                            break;
                                        }
                                    }
                                } else {
                                    engine.set_break_cache();
                                    println!("{}✓ Next request will skip prompt cache\x1b[0m", theme::c_ok());
                                }
                            }
                            CommandResult::Rewind { turns } => {
                                let n: usize = if turns.is_empty() {
                                    1
                                } else {
                                    match turns.parse() {
                                        Ok(v) if v > 0 => v,
                                        _ => {
                                            println!("Usage: /rewind [N]  (N = number of turns to rewind, default 1)");
                                            continue;
                                        }
                                    }
                                };
                                let (removed, remaining) = engine.rewind_turns(n).await;
                                if removed == 0 {
                                    println!("Nothing to rewind.");
                                } else {
                                    println!(
                                        "{}✓ Rewound {} turn{} ({} messages remaining)\x1b[0m",
                                        theme::c_ok(),
                                        removed,
                                        if removed == 1 { "" } else { "s" },
                                        remaining,
                                    );
                                }
                            }
                            CommandResult::Fast { toggle } => {
                                let state = engine.state();
                                let current = state.read().await.model.clone();
                                let fast_model = claude_core::model::small_fast_model();

                                if toggle.eq_ignore_ascii_case("off") {
                                    // Restore to default (sonnet)
                                    let default = claude_core::model::resolve_model_string("sonnet");
                                    if current == default {
                                        println!("Already on default model: {}", claude_core::model::display_name_any(&default));
                                    } else {
                                        state.write().await.model = default.clone();
                                        println!(
                                            "{}✓ Switched back to: {} ({})\x1b[0m",
                                            theme::c_ok(),
                                            claude_core::model::display_name_any(&default),
                                            default,
                                        );
                                    }
                                } else {
                                    // Toggle: if already on fast model, switch to sonnet; else switch to fast
                                    if current == fast_model {
                                        let default = claude_core::model::resolve_model_string("sonnet");
                                        state.write().await.model = default.clone();
                                        println!(
                                            "{}✓ Fast mode off → {} ({})\x1b[0m",
                                            theme::c_ok(),
                                            claude_core::model::display_name_any(&default),
                                            default,
                                        );
                                    } else {
                                        state.write().await.model = fast_model.clone();
                                        println!(
                                            "{}✓ Fast mode on → {} ({})\x1b[0m",
                                            theme::c_ok(),
                                            claude_core::model::display_name_any(&fast_model),
                                            fast_model,
                                        );
                                    }
                                }
                            }
                            CommandResult::AddDir { path } => {
                                if path.is_empty() {
                                    println!("Usage: /add-dir <path>");
                                    continue;
                                }
                                let dir_path = std::path::Path::new(&path);
                                let dir_path = if dir_path.is_relative() {
                                    cwd.join(dir_path)
                                } else {
                                    dir_path.to_path_buf()
                                };
                                if !dir_path.is_dir() {
                                    println!("{}Directory not found: {}\x1b[0m", theme::c_warn(), dir_path.display());
                                    continue;
                                }
                                // Read directory contents and inject as context
                                let mut ctx = format!("<context source=\"{}\">\n", dir_path.display());
                                let mut file_count = 0u32;
                                if let Ok(entries) = std::fs::read_dir(&dir_path) {
                                    for entry in entries.flatten() {
                                        let p = entry.path();
                                        if p.is_file() {
                                            if let Ok(content) = std::fs::read_to_string(&p) {
                                                let name = p.file_name().unwrap_or_default().to_string_lossy();
                                                ctx.push_str(&format!("--- {} ---\n{}\n\n", name, content.trim()));
                                                file_count += 1;
                                            }
                                        }
                                    }
                                }
                                ctx.push_str("</context>");
                                // Inject as system prompt context update
                                engine.update_system_prompt_context(&ctx).await;
                                println!(
                                    "{}✓ Added {} file{} from {}\x1b[0m",
                                    theme::c_ok(),
                                    file_count,
                                    if file_count == 1 { "" } else { "s" },
                                    dir_path.display(),
                                );
                            }
                            CommandResult::Summary => {
                                handle_summary(&engine).await;
                            }
                            CommandResult::Rename { name } => {
                                if name.is_empty() {
                                    println!("{}Usage: /rename <new name>\x1b[0m", theme::c_warn());
                                } else {
                                    match engine.rename_session(&name).await {
                                        Ok(_) => println!("{}✓ Session renamed to '{}'\x1b[0m", theme::c_ok(), name),
                                        Err(e) => eprintln!("{}Rename failed: {}\x1b[0m", theme::c_err(), e),
                                    }
                                }
                            }
                            CommandResult::Copy => {
                                let state = engine.state().read().await;
                                // Find the last assistant text content
                                let text = state.messages.iter().rev().find_map(|m| {
                                    if let claude_core::message::Message::Assistant(a) = m {
                                        a.content.iter().find_map(|b| {
                                            if let claude_core::message::ContentBlock::Text { text } = b {
                                                Some(text.clone())
                                            } else {
                                                None
                                            }
                                        })
                                    } else {
                                        None
                                    }
                                });
                                drop(state);
                                if let Some(text) = text {
                                    match copy_to_clipboard(&text) {
                                        Ok(_) => println!("{}✓ Copied to clipboard ({} chars)\x1b[0m", theme::c_ok(), text.len()),
                                        Err(e) => eprintln!("{}Copy failed: {}\x1b[0m", theme::c_err(), e),
                                    }
                                } else {
                                    println!("{}No assistant response to copy.\x1b[0m", theme::c_warn());
                                }
                            }
                            CommandResult::Share => {
                                let state = engine.state().read().await;
                                let mut md = String::from("# Claude Code Session\n\n");
                                for msg in &state.messages {
                                    match msg {
                                        claude_core::message::Message::User(u) => {
                                            md.push_str("## User\n\n");
                                            for block in &u.content {
                                                if let claude_core::message::ContentBlock::Text { text } = block {
                                                    md.push_str(text);
                                                    md.push_str("\n\n");
                                                }
                                            }
                                        }
                                        claude_core::message::Message::Assistant(a) => {
                                            md.push_str("## Assistant\n\n");
                                            for block in &a.content {
                                                if let claude_core::message::ContentBlock::Text { text } = block {
                                                    md.push_str(text);
                                                    md.push_str("\n\n");
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                drop(state);
                                let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
                                let filename = format!("claude-session-{ts}.md");
                                match std::fs::write(&filename, &md) {
                                    Ok(_) => println!("{}✓ Session exported to {filename} ({} bytes)\x1b[0m", theme::c_ok(), md.len()),
                                    Err(e) => eprintln!("{}Export failed: {e}\x1b[0m", theme::c_err()),
                                }
                            }
                            CommandResult::Files { pattern } => {
                                let cwd = std::env::current_dir().unwrap_or_default();
                                match std::fs::read_dir(&cwd) {
                                    Ok(entries) => {
                                        let mut items: Vec<_> = entries
                                            .flatten()
                                            .filter(|e| {
                                                if pattern.is_empty() {
                                                    true
                                                } else {
                                                    e.file_name().to_string_lossy().contains(pattern.as_str())
                                                }
                                            })
                                            .collect();
                                        items.sort_by_key(|e| e.file_name());
                                        let mut count = 0;
                                        for entry in &items {
                                            let name = entry.file_name();
                                            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                                                println!("  \x1b[1;34m{}/\x1b[0m", name.to_string_lossy());
                                            } else {
                                                println!("  {}", name.to_string_lossy());
                                            }
                                            count += 1;
                                        }
                                        if count == 0 {
                                            println!("{}No files matching '{pattern}'\x1b[0m", theme::c_warn());
                                        } else {
                                            println!("\x1b[2m({count} items in {})\x1b[0m", cwd.display());
                                        }
                                    }
                                    Err(e) => eprintln!("{}Cannot read directory: {e}\x1b[0m", theme::c_err()),
                                }
                            }
                            CommandResult::Env => {
                                println!("\x1b[1mEnvironment\x1b[0m");
                                println!("  OS:          {}", std::env::consts::OS);
                                println!("  Arch:        {}", std::env::consts::ARCH);
                                println!("  CWD:         {}", std::env::current_dir().unwrap_or_default().display());
                                println!("  Version:     v{}", env!("CARGO_PKG_VERSION"));
                                if let Ok(home) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
                                    println!("  Home:        {home}");
                                }
                                let state = engine.state().read().await;
                                println!("  Model:       {}", state.model);
                                println!("  Messages:    {}", state.messages.len());
                                drop(state);
                                if let Ok(shell) = std::env::var("SHELL").or_else(|_| std::env::var("COMSPEC")) {
                                    println!("  Shell:       {shell}");
                                }
                                if let Ok(term) = std::env::var("TERM") {
                                    println!("  Terminal:    {term}");
                                }
                            }
                            CommandResult::Vim { toggle } => {
                                let enabled = match toggle.to_lowercase().as_str() {
                                    "" | "on" | "true" | "1" => true,
                                    "off" | "false" | "0" => false,
                                    _ => {
                                        println!("Usage: /vim [on|off]");
                                        continue;
                                    }
                                };
                                if enabled {
                                    println!("{}Vim mode enabled\x1b[0m (note: basic vim keybindings are a work in progress)", theme::c_ok());
                                } else {
                                    println!("{}Vim mode disabled\x1b[0m — normal editing mode active", theme::c_ok());
                                }
                            }
                            CommandResult::Image { path } => {
                                if path.is_empty() {
                                    println!("Usage: /image <path>  — attach an image to the next message");
                                    println!("       Tip: you can also type @path/to/image.png inline");
                                    println!("       Tip: press Alt+V to paste from clipboard");
                                    continue;
                                }
                                let img_path = std::path::Path::new(&path);
                                let img_path = if img_path.is_relative() {
                                    cwd.join(img_path)
                                } else {
                                    img_path.to_path_buf()
                                };
                                match claude_core::image::read_image_file(&img_path) {
                                    Ok(block) => {
                                        // Queue as a pending image for the next user message
                                        pending_images.push(block);
                                        println!(
                                            "{}✓ Image queued: {} ({} image{} pending)\x1b[0m",
                                            theme::c_ok(),
                                            img_path.file_name().unwrap_or_default().to_string_lossy(),
                                            pending_images.len(),
                                            if pending_images.len() == 1 { "" } else { "s" },
                                        );
                                    }
                                    Err(e) => {
                                        eprintln!("{}Image error: {e}\x1b[0m", theme::c_err());
                                    }
                                }
                            }
                            CommandResult::Stickers => {
                                let url = "https://www.stickermule.com/claudecode";
                                println!("Opening sticker page: {url}");
                                let _ = opener::open(url);
                            }
                            CommandResult::Effort { level } => {
                                let valid = ["low", "medium", "high", "max", "auto"];
                                if level.is_empty() {
                                    println!("Current effort: \x1b[1mauto\x1b[0m");
                                    println!("Options: {}", valid.join(", "));
                                } else if valid.contains(&level.to_lowercase().as_str()) {
                                    let lvl = level.to_lowercase();
                                    println!("{}Effort set to: {}\x1b[0m", theme::c_ok(), lvl);
                                } else {
                                    println!("Invalid effort level: '{level}'");
                                    println!("Options: {}", valid.join(", "));
                                }
                            }
                            CommandResult::Tag { name } => {
                                if name.is_empty() {
                                    println!("Usage: /tag <name>  — add a searchable tag to the session");
                                } else {
                                    println!("{}Tagged session: {name}\x1b[0m", theme::c_ok());
                                }
                            }
                            CommandResult::ReleaseNotes => {
                                println!("\x1b[1mClaude Code (Rust) v{}\x1b[0m", env!("CARGO_PKG_VERSION"));
                                println!();
                                println!("Recent changes:");
                                println!("  • Full cursor position tracking with word navigation");
                                println!("  • /share, /files, /env, /vim, /effort, /tag commands");
                                println!("  • /fast model toggle, /add-dir context directories");
                                println!("  • /rewind, /summary, /rename, /copy commands");
                                println!("  • Syntax-highlighted unified diffs (syntect)");
                                println!("  • Ctrl+R history search, multiline paste");
                                println!("  • 5 terminal themes (dark/light/dark-ansi/solarized/daltonize)");
                                println!("  • Plan mode with structured planning");
                                println!("  • Computer Use via MCP (desktop automation)");
                                println!("  • Swarm mode (multi-agent via kameo)");
                                println!("  • 55+ slash commands, 52+ tools");
                                println!();
                                println!("Source: https://github.com/anthropics/claude-code");
                            }
                            CommandResult::Feedback { text } => {
                                // Save feedback to ~/.claude/feedback.log
                                let feedback_path = dirs::home_dir()
                                    .map(|h| h.join(".claude").join("feedback.log"))
                                    .unwrap_or_else(|| std::path::PathBuf::from("feedback.log"));
                                if let Some(parent) = feedback_path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                let entry = format!("[{timestamp}] {text}\n");
                                match std::fs::OpenOptions::new()
                                    .create(true)
                                    .append(true)
                                    .open(&feedback_path)
                                {
                                    Ok(mut f) => {
                                        use std::io::Write;
                                        let _ = f.write_all(entry.as_bytes());
                                        println!("{}Thank you for your feedback! Saved to {}\x1b[0m",
                                            theme::c_ok(), feedback_path.display());
                                    }
                                    Err(e) => {
                                        eprintln!("{}Could not save feedback: {}\x1b[0m", theme::c_err(), e);
                                    }
                                }
                            }
                            CommandResult::Stats => {
                                handle_stats_command(&engine, session_start).await;
                            }
                        }
                    }
                    continue;
                }

                // Non-slash input: InputReader handles multiline via
                // Shift+Enter / Alt+Enter / bracketed paste.
                let input = line.trim();
                if input.is_empty() { continue; }
                rl.add_history(input);

                // Check auto-compact before submitting
                if engine.should_auto_compact().await {
                    println!("{}[Context limit approaching — auto-compacting…]\x1b[0m", theme::c_warn());
                    if let Err(e) = engine.compact("auto", None).await {
                        eprintln!("{}Auto-compact failed: {}\x1b[0m", theme::c_err(), e);
                    } else {
                        println!("{}[Auto-compact complete]\x1b[0m", theme::c_ok());
                    }
                }

                // Auto-reload config if watcher detected changes
                if let Some(ref mut watcher) = config_watcher {
                    let changes = watcher.drain();
                    if !changes.is_empty() {
                        let changed_files: Vec<String> = changes
                            .iter()
                            .map(|e| match e {
                                claude_core::file_watcher::ConfigChangeEvent::ClaudeMd(p) => {
                                    format!("CLAUDE.md ({})", p.display())
                                }
                                claude_core::file_watcher::ConfigChangeEvent::Settings(p) => {
                                    format!("settings.json ({})", p.display())
                                }
                            })
                            .collect();
                        println!(
                            "\x1b[2m[Config changed: {} — reloading…]\x1b[0m",
                            changed_files.join(", ")
                        );
                        handle_reload_context(&engine, &cwd).await;
                    }
                }

                let model = { engine.state().read().await.model.clone() };

                // Extract @image.png references and @https://url references from input
                let (text, inline_images, url_refs) = claude_core::image::extract_image_refs(input);

                // Fetch URL-referenced images asynchronously
                let mut url_images: Vec<claude_core::message::ContentBlock> = Vec::new();
                for url in &url_refs {
                    match fetch_image_url(url).await {
                        Ok(block) => {
                            println!("\x1b[2m📎 Fetched image: {url}\x1b[0m");
                            url_images.push(block);
                        }
                        Err(e) => {
                            eprintln!("{}Failed to fetch image {url}: {e}\x1b[0m", theme::c_warn());
                        }
                    }
                }

                // Merge pending images (from /image command) with inline @references and URL images
                let all_images: Vec<claude_core::message::ContentBlock> = {
                    let mut combined = std::mem::take(&mut pending_images);
                    combined.extend(inline_images);
                    combined.extend(url_images);
                    combined
                };

                // Submit via bus (preferred) or direct engine (fallback)
                if let Some(ref mut client) = client {
                    // Bus-based path: send request → render notifications
                    let bus_images: Vec<claude_bus::ImageAttachment> = all_images.iter().filter_map(|block| {
                        if let claude_core::message::ContentBlock::Image { source } = block {
                            Some(claude_bus::ImageAttachment {
                                data: source.data.clone(),
                                media_type: source.media_type.clone(),
                            })
                        } else {
                            None
                        }
                    }).collect();

                    if !bus_images.is_empty() {
                        println!(
                            "\x1b[2m📎 {} image{} attached\x1b[0m",
                            bus_images.len(),
                            if bus_images.len() == 1 { "" } else { "s" }
                        );
                    }

                    let request = AgentRequest::Submit { text, images: bus_images };

                    if let Err(e) = client.send_request(request) {
                        eprintln!("{}Failed to send request: {}\x1b[0m", theme::c_err(), e);
                    } else {
                        // ESC listener for abort during bus-based rendering
                        let _esc_guard = spawn_esc_listener(engine.abort_signal());
                        // Render notifications until TurnComplete (10min per-notification timeout)
                        let mut renderer = OutputRenderer::new(&model);
                        let render_timeout = Duration::from_secs(600);
                        while let Some(notification) = recv_with_timeout(client, render_timeout).await {
                            if engine.abort_signal().is_aborted() {
                                break;
                            }
                            let done = renderer.render(notification, Some(engine.cost_tracker()));
                            if done {
                                break;
                            }
                        }
                        // Handle abort in bus path (same as direct path)
                        if engine.abort_signal().is_aborted() {
                            eprintln!("{}⏹ Interrupted\x1b[0m", theme::c_warn());
                            engine.abort_signal().reset();
                            let _ = engine.save_session().await;
                            turns_since_save = 0;
                        }
                    }
                } else {
                    // Direct engine path (legacy fallback)
                    let stream = if all_images.is_empty() {
                        engine.submit(&text).await
                    } else {
                        let img_count = all_images.len();
                        println!(
                            "\x1b[2m📎 {} image{} attached\x1b[0m",
                            img_count,
                            if img_count == 1 { "" } else { "s" }
                        );
                        let mut content = Vec::new();
                        if !text.is_empty() {
                            content.push(claude_core::message::ContentBlock::Text { text });
                        }
                        content.extend(all_images);
                        engine.submit_with_content(content).await
                    };

                    if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker()), Some(&engine.abort_signal())).await {
                        if engine.abort_signal().is_aborted() {
                            eprintln!("{}⏹ Interrupted\x1b[0m", theme::c_warn());
                            engine.abort_signal().reset();
                            let _ = engine.save_session().await;
                            turns_since_save = 0;
                        } else {
                            eprintln!("{}Error: {}\x1b[0m", theme::c_err(), e);
                        }
                    }
                }
                // Reset abort signal after each turn
                if engine.abort_signal().is_aborted() {
                    engine.abort_signal().reset();
                }

                // Show turn stats + context usage warning
                print_turn_stats(&engine).await;

                // Context usage warning (80% threshold)
                if let Some(pct) = engine.context_usage_percent().await {
                    if pct >= 90 {
                        eprintln!("{}⚠ Context {pct}% full — consider /compact or /clear\x1b[0m", theme::c_err());
                    } else if pct >= 80 {
                        eprintln!("{}⚠ Context {pct}% full\x1b[0m", theme::c_warn());
                    }
                }

                // Periodic session checkpoint to prevent data loss on crash
                turns_since_save += 1;
                if turns_since_save >= CHECKPOINT_INTERVAL {
                    turns_since_save = 0;
                    if let Err(e) = engine.save_session().await {
                        tracing::debug!("Session checkpoint failed: {}", e);
                    } else {
                        tracing::debug!("Session checkpoint saved");
                    }
                }

                // In coordinator mode: drain background agent notifications and
                // re-submit them so the coordinator can react to completed tasks.
                if engine.is_coordinator() {
                    const MAX_NOTIFICATION_ROUNDS: u32 = 10;
                    let mut rounds = 0;
                    loop {
                        let notifications = engine.drain_notifications().await;
                        if notifications.is_empty() || rounds >= MAX_NOTIFICATION_ROUNDS {
                            break;
                        }
                        rounds += 1;
                        for notif in &notifications {
                            if let claude_core::message::Message::User(u) = notif {
                                // Concatenate all text blocks from the notification
                                let text: String = u.content.iter().filter_map(|b| {
                                    if let claude_core::message::ContentBlock::Text { text } = b {
                                        Some(text.as_str())
                                    } else {
                                        None
                                    }
                                }).collect::<Vec<_>>().join("\n");
                                if text.is_empty() { continue; }
                                eprintln!("{}[Task notification received]\x1b[0m", theme::c_warn());
                                let stream = engine.submit(&text).await;
                                if let Err(e) = print_stream(stream, &model, Some(engine.cost_tracker()), Some(&engine.abort_signal())).await {
                                    eprintln!("{}Error: {}\x1b[0m", theme::c_err(), e);
                                }
                            }
                        }
                    }
                }
            }
            Ok(InputResult::Interrupted) => { continue; }
            Ok(InputResult::Eof) => { println!("Goodbye!"); break; }
            Err(err) => {
                eprintln!("{}Input error: {}\x1b[0m", theme::c_err(), err);
                break;
            }
        }
    }

    // Save persistent history
    if let Some(ref path) = hist_path {
        rl.save_history(path);
    }

    // Auto-save session on exit (if there's history)
    let has_messages = { engine.state().read().await.messages.len() > 1 };
    if has_messages {
        if let Err(e) = engine.save_session().await {
            eprintln!("\x1b[2m(Session auto-save failed: {})\x1b[0m", e);
        } else {
            eprintln!("\x1b[2m(Session saved: {})\x1b[0m", &engine.session_id()[..8]);
        }
    }

    Ok(())
}

/// Display token usage and cost after each turn.
async fn print_turn_stats(engine: &QueryEngine) {
    let state = engine.state().read().await;
    let cost = engine.cost_tracker();
    let total_cost = cost.total_usd();

    let input_k = format_compact_tokens(state.total_input_tokens);
    let output_k = format_compact_tokens(state.total_output_tokens);

    if total_cost > 0.0 {
        eprintln!(
            "\x1b[2m  tokens: {}↓ {}↑ · cost: ${:.4} · turns: {}\x1b[0m",
            input_k, output_k, total_cost, state.turn_count
        );
    }
}

/// Format tokens compactly: 1234 → "1.2K", 12345 → "12K", 1234567 → "1.2M"
fn format_compact_tokens(n: u64) -> String {
    if n < 1_000 {
        format!("{}", n)
    } else if n < 100_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}K", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Truncate a path for display (keep last components within `max_len` chars).
fn truncate_path(path: &std::path::Path, max_len: usize) -> String {
    let s = path.display().to_string();
    if s.chars().count() <= max_len {
        return s;
    }
    let skip = s.chars().count() - max_len + 1;
    let tail: String = s.chars().skip(skip).collect();
    format!("…{}", tail)
}

/// Copy text to the system clipboard (cross-platform).
fn copy_to_clipboard(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    #[cfg(target_os = "windows")]
    let mut child = Command::new("clip")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    #[cfg(target_os = "macos")]
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    #[cfg(target_os = "linux")]
    let mut child = {
        // Try xclip first, fall back to xsel
        Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .or_else(|_| Command::new("xsel")
                .args(["--clipboard", "--input"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn())?
    };

    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes())?;
    }
    child.wait()?;
    Ok(())
}

/// Display detailed session statistics for /stats command.
async fn handle_stats_command(engine: &QueryEngine, session_start: std::time::Instant) {
    let state = engine.state().read().await;
    let cost = engine.cost_tracker();
    let total_cost = cost.total_usd();
    let elapsed = session_start.elapsed();

    let ok = theme::c_ok();
    let bold = "\x1b[1m";
    let dim = "\x1b[2m";
    let reset = "\x1b[0m";

    println!("{bold}Session Statistics{reset}");
    println!("{dim}─────────────────────────────────────{reset}");

    // Session timing
    let secs = elapsed.as_secs();
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        println!("{ok}Session duration:{reset} {:02}h {:02}m {:02}s", h, m, s);
    } else {
        println!("{ok}Session duration:{reset} {:02}m {:02}s", m, s);
    }
    println!("{ok}Turns:{reset}           {}", state.turn_count);

    // Token usage
    println!("\n{bold}Token Usage{reset}");
    println!("{dim}─────────────────────────────────────{reset}");
    println!("{ok}Input tokens:{reset}    {}", state.total_input_tokens);
    println!("{ok}Output tokens:{reset}   {}", state.total_output_tokens);
    let total_tokens = state.total_input_tokens + state.total_output_tokens;
    println!("{ok}Total tokens:{reset}    {}", total_tokens);
    if total_cost > 0.0 {
        println!("{ok}Total cost:{reset}      ${:.4}", total_cost);
    }

    // Per-model breakdown
    if !state.model_usage.is_empty() {
        println!("\n{bold}Model Breakdown{reset}");
        println!("{dim}─────────────────────────────────────{reset}");
        let mut models: Vec<_> = state.model_usage.iter().collect();
        models.sort_by_key(|(name, _)| name.as_str());
        for (model, usage) in &models {
            let display = claude_core::model::display_name_any(model);
            println!("{ok}{display}{reset}");
            println!("  API calls:    {}", usage.api_calls);
            println!("  Input:        {}", usage.input_tokens);
            println!("  Output:       {}", usage.output_tokens);
            if usage.cache_read_tokens > 0 || usage.cache_creation_tokens > 0 {
                println!("  Cache read:   {}", usage.cache_read_tokens);
                println!("  Cache write:  {}", usage.cache_creation_tokens);
            }
            if usage.cost_usd > 0.0 {
                println!("  Cost:         ${:.4}", usage.cost_usd);
            }
        }
    }

    // Code changes
    if state.total_lines_added > 0 || state.total_lines_removed > 0 {
        println!("\n{bold}Code Changes{reset}");
        println!("{dim}─────────────────────────────────────{reset}");
        println!("{ok}Lines added:{reset}     +{}", state.total_lines_added);
        println!("{ok}Lines removed:{reset}   -{}", state.total_lines_removed);
    }

    // Timing breakdown
    if state.total_api_duration_ms > 0 || state.total_tool_duration_ms > 0 {
        println!("\n{bold}Timing{reset}");
        println!("{dim}─────────────────────────────────────{reset}");
        if state.total_api_duration_ms > 0 {
            let api_s = state.total_api_duration_ms as f64 / 1000.0;
            println!("{ok}API time:{reset}        {:.2}s", api_s);
        }
        if state.total_tool_duration_ms > 0 {
            let tool_s = state.total_tool_duration_ms as f64 / 1000.0;
            println!("{ok}Tool time:{reset}       {:.2}s", tool_s);
        }
    }

    // Errors
    if state.total_errors > 0 {
        let err = theme::c_err();
        println!("\n{bold}Errors{reset}");
        println!("{dim}─────────────────────────────────────{reset}");
        println!("{err}Total errors:{reset}    {}", state.total_errors);
        if !state.error_counts.is_empty() {
            let mut errs: Vec<_> = state.error_counts.iter().collect();
            errs.sort_by_key(|(_, &c)| std::cmp::Reverse(c));
            for (kind, count) in errs.iter().take(5) {
                println!("  {kind}: {count}");
            }
        }
    }
}

/// Fetch an image from a URL and return a `ContentBlock::Image`.
///
/// Validates: content-type is a supported image type, file size ≤ 20 MB.
async fn fetch_image_url(url: &str) -> anyhow::Result<claude_core::message::ContentBlock> {
    use anyhow::Context as _;
    use base64::Engine as _;

    let client = reqwest::Client::builder()
        .user_agent(concat!("claude-code-rs/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let response = client.get(url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch image from {url}"))?;

    if !response.status().is_success() {
        anyhow::bail!("HTTP {} fetching {url}", response.status());
    }

    // Check content-type header
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let media_type = if content_type.contains("png") {
        "image/png"
    } else if content_type.contains("jpeg") || content_type.contains("jpg") {
        "image/jpeg"
    } else if content_type.contains("gif") {
        "image/gif"
    } else if content_type.contains("webp") {
        "image/webp"
    } else {
        // Fall back to extension in URL
        let path = std::path::Path::new(url);
        match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("png") => "image/png",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            _ => anyhow::bail!(
                "Cannot determine image type from URL or Content-Type: {url} ({content_type})"
            ),
        }
    };

    // Read body with size limit
    let bytes = response.bytes().await
        .with_context(|| format!("Failed to read image body from {url}"))?;

    if bytes.len() > 20 * 1024 * 1024 {
        anyhow::bail!("Image from {url} is too large ({} bytes, max 20 MB)", bytes.len());
    }

    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);

    Ok(claude_core::message::ContentBlock::Image {
        source: claude_core::message::ImageSource {
            media_type: media_type.to_string(),
            data,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_compact_tokens_below_1k() {
        assert_eq!(format_compact_tokens(0), "0");
        assert_eq!(format_compact_tokens(1), "1");
        assert_eq!(format_compact_tokens(999), "999");
    }

    #[test]
    fn format_compact_tokens_kilos() {
        assert_eq!(format_compact_tokens(1_000), "1.0K");
        assert_eq!(format_compact_tokens(1_234), "1.2K");
        assert_eq!(format_compact_tokens(15_500), "15.5K");
        assert_eq!(format_compact_tokens(99_999), "100.0K");
    }

    #[test]
    fn format_compact_tokens_large_kilos() {
        assert_eq!(format_compact_tokens(100_000), "100K");
        assert_eq!(format_compact_tokens(500_000), "500K");
        assert_eq!(format_compact_tokens(999_999), "999K");
    }

    #[test]
    fn format_compact_tokens_megas() {
        assert_eq!(format_compact_tokens(1_000_000), "1.0M");
        assert_eq!(format_compact_tokens(1_500_000), "1.5M");
        assert_eq!(format_compact_tokens(12_345_678), "12.3M");
    }

    #[test]
    fn truncate_path_short() {
        let p = std::path::Path::new("src");
        assert_eq!(truncate_path(p, 25), "src");
    }

    #[test]
    fn truncate_path_long() {
        let p = std::path::Path::new("/very/long/path/that/exceeds/limit");
        let result = truncate_path(p, 15);
        assert!(result.starts_with('…'));
        // Display length matters, not byte length
        assert!(result.chars().count() <= 16);
    }
}
