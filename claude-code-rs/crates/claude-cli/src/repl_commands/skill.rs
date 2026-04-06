//! Skill runner — executes a skill as a single-shot sub-agent conversation.

use claude_agent::engine::QueryEngine;
use claude_core::skills::SkillEntry;
use rustyline::DefaultEditor;

use crate::output::print_stream;

/// Run a skill as a single-shot sub-agent conversation.
pub(crate) async fn run_skill(
    parent_engine: &QueryEngine,
    skills: &[SkillEntry],
    name: &str,
    prompt: &str,
    rl: &mut DefaultEditor,
) {
    let skill = match skills.iter().find(|s| s.name == name) {
        Some(s) => s,
        None => { eprintln!("Unknown skill: {}", name); return; }
    };

    let user_prompt = if prompt.is_empty() {
        match rl.readline(&format!("\x1b[1;35m[{}]> \x1b[0m", skill.name)) {
            Ok(p) if !p.trim().is_empty() => p,
            _ => return,
        }
    } else {
        prompt.to_string()
    };

    println!("\x1b[35m[Running skill: {}]\x1b[0m", skill.name);

    let augmented = if skill.system_prompt.is_empty() {
        user_prompt
    } else {
        format!(
            "<skill_context>\n{}\n</skill_context>\n\n{}",
            skill.system_prompt, user_prompt
        )
    };

    if !skill.allowed_tools.is_empty() {
        println!(
            "\x1b[33m  (Skill restricts tools to: {})\x1b[0m",
            skill.allowed_tools.join(", ")
        );
    }

    let model = { parent_engine.state().read().await.model.clone() };
    let stream = parent_engine.submit(&augmented).await;
    if let Err(e) = print_stream(stream, &model, Some(parent_engine.cost_tracker())).await {
        eprintln!("\x1b[31mSkill error: {}\x1b[0m", e);
    }
}
