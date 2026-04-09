//! Bash command risk classifier for permission decisions.
//!
//! Classifies shell commands into risk levels based on their first token
//! and argument patterns. Used by the permission system to auto-approve
//! safe commands and flag dangerous ones.

use serde::{Deserialize, Serialize};

/// Risk level for a shell command, from safest to most dangerous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RiskLevel {
    /// Read-only commands that never modify state (ls, cat, grep).
    Safe,
    /// Commands that modify project files within the working directory.
    ProjectWrite,
    /// Package managers, build tools — modify local project state.
    Build,
    /// Network-accessing commands (curl, wget, ssh).
    Network,
    /// Interpreters that can execute arbitrary code (python, node, eval).
    CodeExec,
    /// System-level mutations (sudo, chmod, chown, service management).
    System,
    /// Catastrophic commands that can destroy data (rm -rf /, mkfs).
    Destructive,
}

impl RiskLevel {
    /// Whether this risk level should be auto-approved in "accept edits" mode.
    pub fn auto_approvable(&self) -> bool {
        matches!(self, RiskLevel::Safe | RiskLevel::ProjectWrite | RiskLevel::Build)
    }

    /// Whether this risk level should always require explicit permission.
    pub fn always_ask(&self) -> bool {
        matches!(self, RiskLevel::CodeExec | RiskLevel::System | RiskLevel::Destructive)
    }

    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "read-only",
            RiskLevel::ProjectWrite => "project write",
            RiskLevel::Build => "build/install",
            RiskLevel::Network => "network access",
            RiskLevel::CodeExec => "code execution",
            RiskLevel::System => "system mutation",
            RiskLevel::Destructive => "destructive",
        }
    }
}

/// Result of classifying a bash command.
#[derive(Debug, Clone)]
pub struct ClassifyResult {
    /// The determined risk level.
    pub risk: RiskLevel,
    /// The base command that was matched (e.g., "git", "rm").
    pub base_command: String,
    /// Human-readable reason for the classification.
    pub reason: &'static str,
}

// ── Command Lists ────────────────────────────────────────────────────────────

/// Read-only commands that never modify state.
const SAFE_COMMANDS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "wc", "file", "stat",
    "du", "df", "ls", "tree", "find", "which", "type", "whereis", "locate",
    "grep", "egrep", "fgrep", "rg", "ag", "ack",
    "echo", "printf", "date", "whoami", "hostname", "uname", "pwd",
    "env", "printenv", "id", "groups",
    "diff", "comm", "cmp", "sort", "uniq", "cut", "tr", "sed", "awk",
    "jq", "yq", "xq",
    "git log", "git show", "git diff", "git status", "git branch",
    "git stash list", "git remote", "git tag", "git rev-parse",
    "git describe", "git blame", "git shortlog",
    "cargo check", "cargo clippy", "cargo doc",
    "npm ls", "npm list", "npm outdated", "npm view",
    "pip list", "pip show", "pip freeze",
    "rustc --version", "node --version", "python --version",
    "man", "help", "info",
    "true", "false", "test",
];

/// Project-scope write commands.
const PROJECT_WRITE_COMMANDS: &[&str] = &[
    "mkdir", "touch", "cp", "mv", "rm", "ln",
    "git add", "git commit", "git stash", "git merge", "git rebase",
    "git checkout", "git switch", "git restore", "git cherry-pick",
    "git fetch", "git pull", "git push",
    "chmod", "patch",
];

/// Build/install commands (modify project deps, not system).
const BUILD_COMMANDS: &[&str] = &[
    "make", "cmake", "ninja",
    "cargo build", "cargo test", "cargo run", "cargo install", "cargo fmt",
    "npm install", "npm ci", "npm run", "npm test", "npm start",
    "yarn", "yarn install", "yarn add", "yarn run",
    "pnpm install", "pnpm add", "pnpm run",
    "bun install", "bun run", "bun test",
    "pip install", "pip3 install", "pipenv install", "poetry install",
    "go build", "go test", "go run", "go install", "go mod",
    "mvn", "gradle", "ant",
    "dotnet build", "dotnet test", "dotnet run",
    "docker build", "docker compose",
    "apt install", "apt-get install", "brew install",
];

/// Network-accessing commands.
const NETWORK_COMMANDS: &[&str] = &[
    "curl", "wget", "http", "httpie",
    "ssh", "scp", "sftp", "rsync",
    "ping", "traceroute", "nslookup", "dig", "host",
    "nc", "netcat", "ncat", "socat",
    "gh", "gh api",
    "docker pull", "docker push",
    "git clone",
];

/// Interpreters / code execution commands (can run arbitrary code).
///
/// Mirrors JS `CROSS_PLATFORM_CODE_EXEC` from `dangerousPatterns.ts`.
const CODE_EXEC_COMMANDS: &[&str] = &[
    // Interpreters
    "python", "python3", "python2", "python3.",
    "node", "deno", "bun", "tsx",
    "ruby", "perl", "php", "lua", "elixir", "julia",
    // Package runners (can execute arbitrary packages)
    "npx", "bunx",
    // Shell re-entry / eval
    "bash", "sh", "zsh", "fish", "dash", "csh", "ksh",
    "eval", "exec", "source",
    "xargs", "env",
    // Remote code execution
    "nohup",
];

/// System-level mutation commands.
const SYSTEM_COMMANDS: &[&str] = &[
    "sudo", "su",
    "chown", "chgrp",
    "systemctl", "service", "launchctl",
    "mount", "umount",
    "iptables", "ufw", "firewall-cmd",
    "useradd", "userdel", "usermod", "groupadd",
    "crontab",
    "reboot", "shutdown", "halt", "poweroff",
    "kill", "killall", "pkill",
    "kubectl", "aws", "gcloud", "gsutil", "az",
];

// ── Classification Logic ─────────────────────────────────────────────────────

/// Extract the base command from a shell command string.
///
/// Handles:
/// - Leading environment variable assignments (`FOO=bar cmd`)
/// - Pipelines (takes first command)
///
/// Does NOT strip `sudo` — that's handled by [`classify`] which
/// elevates sudo-prefixed commands to at least [`RiskLevel::System`].
fn extract_base(command: &str) -> String {
    let trimmed = command.trim();

    // Take first command in a pipeline
    let first = trimmed.split('|').next().unwrap_or(trimmed).trim();

    // Strip leading env assignments only
    let mut words = first.split_whitespace().peekable();
    loop {
        match words.peek() {
            Some(&w) if w.contains('=') && !w.starts_with('-') => { words.next(); }
            _ => break,
        }
    }

    // Reconstruct remaining command
    let remaining: Vec<&str> = words.collect();
    remaining.join(" ").to_lowercase()
}

/// Extract the base command after stripping `sudo` prefix.
fn extract_base_without_sudo(command: &str) -> String {
    let base = extract_base(command);
    if let Some(rest) = base.strip_prefix("sudo ") {
        rest.trim().to_string()
    } else if base == "sudo" {
        String::new()
    } else {
        base
    }
}

/// Classify a shell command by risk level.
pub fn classify(command: &str) -> ClassifyResult {
    let base = extract_base(command);
    if base.is_empty() {
        return ClassifyResult {
            risk: RiskLevel::Safe,
            base_command: String::new(),
            reason: "empty command",
        };
    }

    // Handle sudo: classify the inner command, but elevate to at least System
    if base.starts_with("sudo ") || base == "sudo" {
        let inner = extract_base_without_sudo(command);
        if inner.is_empty() {
            return ClassifyResult {
                risk: RiskLevel::System,
                base_command: "sudo".to_string(),
                reason: "sudo without inner command",
            };
        }
        let mut result = classify_base(&inner);
        // Sudo elevates to at least System, but keep higher risk levels
        result.risk = std::cmp::max(result.risk, RiskLevel::System);
        result.base_command = format!("sudo {}", result.base_command);
        return result;
    }

    classify_base(&base)
}

/// Classify a base command string (already lowercased, no sudo).
fn classify_base(base: &str) -> ClassifyResult {

    // Check code exec first (highest priority after destructive)
    for &pat in CODE_EXEC_COMMANDS {
        if cmd_matches(base, pat) {
            return ClassifyResult {
                risk: RiskLevel::CodeExec,
                base_command: pat.to_string(),
                reason: "interpreter / code execution command",
            };
        }
    }

    // System commands
    for &pat in SYSTEM_COMMANDS {
        if cmd_matches(base, pat) {
            return ClassifyResult {
                risk: RiskLevel::System,
                base_command: pat.to_string(),
                reason: "system-level mutation command",
            };
        }
    }

    // Network commands
    for &pat in NETWORK_COMMANDS {
        if cmd_matches(base, pat) {
            return ClassifyResult {
                risk: RiskLevel::Network,
                base_command: pat.to_string(),
                reason: "network-accessing command",
            };
        }
    }

    // Build commands (check before project write since some overlap)
    for &pat in BUILD_COMMANDS {
        if cmd_matches(base, pat) {
            return ClassifyResult {
                risk: RiskLevel::Build,
                base_command: pat.to_string(),
                reason: "build or package install command",
            };
        }
    }

    // Project write commands
    for &pat in PROJECT_WRITE_COMMANDS {
        if cmd_matches(base, pat) {
            return ClassifyResult {
                risk: RiskLevel::ProjectWrite,
                base_command: pat.to_string(),
                reason: "project file modification",
            };
        }
    }

    // Safe / read-only commands
    for &pat in SAFE_COMMANDS {
        if cmd_matches(base, pat) {
            return ClassifyResult {
                risk: RiskLevel::Safe,
                base_command: pat.to_string(),
                reason: "read-only command",
            };
        }
    }

    // Unknown command defaults to Network risk (conservative)
    ClassifyResult {
        risk: RiskLevel::Network,
        base_command: base.split_whitespace().next().unwrap_or("").to_string(),
        reason: "unknown command (treated as potentially risky)",
    }
}

/// Check if a base command matches a pattern.
///
/// Multi-word patterns (e.g., "git log") match the start of the command.
/// Single-word patterns match the first word exactly.
fn cmd_matches(base: &str, pattern: &str) -> bool {
    if pattern.contains(' ') {
        // Multi-word: match as prefix with word boundary
        base.starts_with(pattern)
            && (base.len() == pattern.len()
                || base.as_bytes().get(pattern.len()).is_none_or(|b| b.is_ascii_whitespace()))
    } else {
        // Single word: match first token exactly
        let first = base.split_whitespace().next().unwrap_or("");
        first == pattern
    }
}

// ── Dangerous permission patterns ────────────────────────────────────────────

/// Command prefixes that would allow arbitrary code execution if auto-approved.
/// Permission rules matching these patterns should be stripped in auto-approve modes.
const DANGEROUS_BASH_PREFIXES: &[&str] = &[
    // Interpreters
    "python", "python3", "python2", "node", "ruby", "perl", "php", "lua",
    "bash", "sh", "zsh", "fish", "ksh", "csh", "dash",
    // Package runners
    "npm run", "yarn run", "npx", "bunx", "pnpm exec",
    // Execution / escalation
    "eval", "exec", "env", "xargs", "sudo", "su",
    // Remote
    "ssh", "curl", "wget",
];

/// Command prefixes that would allow arbitrary code execution in PowerShell.
const DANGEROUS_POWERSHELL_PREFIXES: &[&str] = &[
    // Nested shells
    "pwsh", "powershell", "cmd", "wsl",
    // Evaluators
    "iex", "invoke-expression", "icm", "invoke-command",
    // Process spawners
    "start-process", "start-job",
    // .NET escapes
    "add-type", "new-object",
    // Scripting
    "python", "python3", "node", "ruby", "perl", "php",
];

/// Check if a permission rule pattern is dangerous (would bypass security).
///
/// A pattern is dangerous if it would auto-allow commands that can execute
/// arbitrary code (interpreters, package runners, shells, sudo, etc).
///
/// Returns `Some(reason)` if dangerous, `None` if safe.
pub fn is_dangerous_permission(tool_name: &str, pattern: &str) -> Option<&'static str> {
    let lower_tool = tool_name.to_lowercase();
    let lower_pat = pattern.to_lowercase();

    if lower_tool.contains("bash") || lower_tool == "shell" {
        for &prefix in DANGEROUS_BASH_PREFIXES {
            if lower_pat.starts_with(prefix)
                || lower_pat == format!("{}*", prefix)
                || lower_pat == format!("{}:*", prefix)
            {
                return Some("allows arbitrary code execution via shell");
            }
        }
        // Wildcard-only patterns are dangerous
        if lower_pat == "*" || lower_pat == "**" {
            return Some("wildcard allows all commands");
        }
    }

    if lower_tool.contains("powershell") {
        for &prefix in DANGEROUS_POWERSHELL_PREFIXES {
            if lower_pat.starts_with(prefix)
                || lower_pat == format!("{}*", prefix)
                || lower_pat == format!("{}:*", prefix)
            {
                return Some("allows arbitrary code execution via PowerShell");
            }
        }
        if lower_pat == "*" || lower_pat == "**" {
            return Some("wildcard allows all commands");
        }
    }

    // Agent wildcard is dangerous — bypasses delegation safety
    if lower_tool.contains("agent") && (lower_pat == "*" || lower_pat.is_empty()) {
        return Some("wildcard agent permission bypasses delegation safety");
    }

    None
}

/// Filter out dangerous permission rules from a rule set.
/// Returns the safe subset plus a list of stripped rule descriptions.
pub fn strip_dangerous_rules(
    rules: &[crate::permissions::PermissionRule],
) -> (Vec<crate::permissions::PermissionRule>, Vec<String>) {
    let mut safe = Vec::new();
    let mut stripped = Vec::new();

    for rule in rules {
        if rule.behavior != crate::permissions::PermissionBehavior::Allow {
            safe.push(rule.clone());
            continue;
        }
        if let Some(ref pattern) = rule.pattern {
            if let Some(reason) = is_dangerous_permission(&rule.tool_name, pattern) {
                stripped.push(format!(
                    "Stripped rule: {}({}) — {}",
                    rule.tool_name, pattern, reason
                ));
                continue;
            }
        }
        safe.push(rule.clone());
    }

    (safe, stripped)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_commands() {
        assert_eq!(classify("ls -la").risk, RiskLevel::Safe);
        assert_eq!(classify("cat README.md").risk, RiskLevel::Safe);
        assert_eq!(classify("grep -r foo src/").risk, RiskLevel::Safe);
        assert_eq!(classify("git log --oneline").risk, RiskLevel::Safe);
        assert_eq!(classify("git diff HEAD~1").risk, RiskLevel::Safe);
        assert_eq!(classify("echo hello").risk, RiskLevel::Safe);
        assert_eq!(classify("wc -l src/*.rs").risk, RiskLevel::Safe);
        assert_eq!(classify("jq .name package.json").risk, RiskLevel::Safe);
        assert_eq!(classify("cargo check").risk, RiskLevel::Safe);
    }

    #[test]
    fn project_write_commands() {
        assert_eq!(classify("mkdir -p src/new").risk, RiskLevel::ProjectWrite);
        assert_eq!(classify("git add .").risk, RiskLevel::ProjectWrite);
        assert_eq!(classify("git commit -m 'msg'").risk, RiskLevel::ProjectWrite);
        assert_eq!(classify("rm old_file.txt").risk, RiskLevel::ProjectWrite);
        assert_eq!(classify("cp a.txt b.txt").risk, RiskLevel::ProjectWrite);
        assert_eq!(classify("git push origin main").risk, RiskLevel::ProjectWrite);
    }

    #[test]
    fn build_commands() {
        assert_eq!(classify("cargo build").risk, RiskLevel::Build);
        assert_eq!(classify("cargo test --workspace").risk, RiskLevel::Build);
        assert_eq!(classify("npm install").risk, RiskLevel::Build);
        assert_eq!(classify("npm run test").risk, RiskLevel::Build);
        assert_eq!(classify("make all").risk, RiskLevel::Build);
        assert_eq!(classify("pip install requests").risk, RiskLevel::Build);
        assert_eq!(classify("go test ./...").risk, RiskLevel::Build);
    }

    #[test]
    fn network_commands() {
        assert_eq!(classify("curl https://api.example.com").risk, RiskLevel::Network);
        assert_eq!(classify("wget https://example.com/file").risk, RiskLevel::Network);
        assert_eq!(classify("ssh user@host").risk, RiskLevel::Network);
        assert_eq!(classify("gh api /repos").risk, RiskLevel::Network);
        assert_eq!(classify("ping 8.8.8.8").risk, RiskLevel::Network);
    }

    #[test]
    fn code_exec_commands() {
        assert_eq!(classify("python3 script.py").risk, RiskLevel::CodeExec);
        assert_eq!(classify("node index.js").risk, RiskLevel::CodeExec);
        assert_eq!(classify("npx create-react-app myapp").risk, RiskLevel::CodeExec);
        assert_eq!(classify("eval 'echo hi'").risk, RiskLevel::CodeExec);
        assert_eq!(classify("bash -c 'rm -rf /'").risk, RiskLevel::CodeExec);
        assert_eq!(classify("ruby script.rb").risk, RiskLevel::CodeExec);
        assert_eq!(classify("xargs rm").risk, RiskLevel::CodeExec);
    }

    #[test]
    fn system_commands() {
        assert_eq!(classify("sudo apt update").risk, RiskLevel::System);
        assert_eq!(classify("systemctl restart nginx").risk, RiskLevel::System);
        assert_eq!(classify("kill -9 1234").risk, RiskLevel::System);
        assert_eq!(classify("kubectl apply -f pod.yaml").risk, RiskLevel::System);
        assert_eq!(classify("aws s3 cp file s3://bucket/").risk, RiskLevel::System);
    }

    #[test]
    fn unknown_commands_are_conservative() {
        let r = classify("some_custom_binary --flag");
        assert_eq!(r.risk, RiskLevel::Network);
        assert_eq!(r.reason, "unknown command (treated as potentially risky)");
    }

    #[test]
    fn strips_env_vars_and_sudo() {
        assert_eq!(classify("FOO=bar ls").risk, RiskLevel::Safe);
        // sudo elevates to at least System
        assert_eq!(classify("sudo ls").risk, RiskLevel::System);
        assert_eq!(classify("FOO=1 BAR=2 sudo grep -r x").risk, RiskLevel::System);
    }

    #[test]
    fn pipeline_uses_first_command() {
        assert_eq!(classify("cat file.txt | grep foo").risk, RiskLevel::Safe);
        assert_eq!(classify("python3 script.py | head").risk, RiskLevel::CodeExec);
    }

    #[test]
    fn risk_level_ordering() {
        assert!(RiskLevel::Safe < RiskLevel::ProjectWrite);
        assert!(RiskLevel::ProjectWrite < RiskLevel::Build);
        assert!(RiskLevel::Build < RiskLevel::Network);
        assert!(RiskLevel::Network < RiskLevel::CodeExec);
        assert!(RiskLevel::CodeExec < RiskLevel::System);
        assert!(RiskLevel::System < RiskLevel::Destructive);
    }

    #[test]
    fn auto_approve_rules() {
        assert!(RiskLevel::Safe.auto_approvable());
        assert!(RiskLevel::ProjectWrite.auto_approvable());
        assert!(RiskLevel::Build.auto_approvable());
        assert!(!RiskLevel::Network.auto_approvable());
        assert!(!RiskLevel::CodeExec.auto_approvable());
        assert!(!RiskLevel::System.auto_approvable());
        assert!(!RiskLevel::Destructive.auto_approvable());
    }

    #[test]
    fn always_ask_rules() {
        assert!(!RiskLevel::Safe.always_ask());
        assert!(!RiskLevel::ProjectWrite.always_ask());
        assert!(!RiskLevel::Build.always_ask());
        assert!(!RiskLevel::Network.always_ask());
        assert!(RiskLevel::CodeExec.always_ask());
        assert!(RiskLevel::System.always_ask());
        assert!(RiskLevel::Destructive.always_ask());
    }

    #[test]
    fn extract_base_handles_edge_cases() {
        assert_eq!(extract_base(""), "");
        assert_eq!(extract_base("   "), "");
        assert_eq!(extract_base("FOO=bar"), "");
        assert_eq!(extract_base("ls"), "ls");
        assert_eq!(extract_base("  ls  -la  "), "ls -la");
    }

    #[test]
    fn cmd_matches_word_boundary() {
        // "git log" should not match "git logging"
        assert!(cmd_matches("git log --oneline", "git log"));
        assert!(!cmd_matches("git logging", "git log"));
        // Single word matches first token
        assert!(cmd_matches("ls -la", "ls"));
        assert!(!cmd_matches("ls -la", "lsof"));
    }

    #[test]
    fn classify_result_has_base_command() {
        let r = classify("git diff HEAD~1");
        assert_eq!(r.base_command, "git diff");
        assert_eq!(r.risk, RiskLevel::Safe);

        let r = classify("python3 -m pytest");
        assert_eq!(r.base_command, "python3");
        assert_eq!(r.risk, RiskLevel::CodeExec);
    }

    #[test]
    fn sudo_strips_to_inner_command() {
        // "sudo ls" → inner is Safe, but elevated by sudo → System
        assert_eq!(classify("sudo ls -la").risk, RiskLevel::System);
        // "sudo systemctl" → inner is System, stays System
        assert_eq!(classify("sudo systemctl restart nginx").risk, RiskLevel::System);
        // "sudo" alone → System (bare sudo)
        assert_eq!(classify("sudo").risk, RiskLevel::System);
        // "sudo python3 script.py" → inner is CodeExec, elevated to System (sudo = root)
        assert_eq!(classify("sudo python3 script.py").risk, RiskLevel::System);
        // "sudo rm file" → inner is ProjectWrite, elevated to System
        assert_eq!(classify("sudo rm file").risk, RiskLevel::System);
    }

    // ── dangerous permission detection ──────────────────────────────

    #[test]
    fn dangerous_bash_patterns() {
        assert!(is_dangerous_permission("Bash", "python*").is_some());
        assert!(is_dangerous_permission("Bash", "node*").is_some());
        assert!(is_dangerous_permission("Bash", "sh*").is_some());
        assert!(is_dangerous_permission("Bash", "sudo*").is_some());
        assert!(is_dangerous_permission("Bash", "eval*").is_some());
        assert!(is_dangerous_permission("Bash", "ssh*").is_some());
        assert!(is_dangerous_permission("Bash", "npm run*").is_some());
        assert!(is_dangerous_permission("Bash", "*").is_some());
    }

    #[test]
    fn safe_bash_patterns() {
        assert!(is_dangerous_permission("Bash", "git*").is_none());
        assert!(is_dangerous_permission("Bash", "cargo*").is_none());
        assert!(is_dangerous_permission("Bash", "ls*").is_none());
        assert!(is_dangerous_permission("Bash", "cat*").is_none());
    }

    #[test]
    fn dangerous_powershell_patterns() {
        assert!(is_dangerous_permission("PowerShell", "iex*").is_some());
        assert!(is_dangerous_permission("PowerShell", "invoke-expression*").is_some());
        assert!(is_dangerous_permission("PowerShell", "cmd*").is_some());
        assert!(is_dangerous_permission("PowerShell", "start-process*").is_some());
        assert!(is_dangerous_permission("PowerShell", "*").is_some());
    }

    #[test]
    fn dangerous_agent_wildcard() {
        assert!(is_dangerous_permission("Agent", "*").is_some());
        assert!(is_dangerous_permission("Agent", "").is_some());
        // Specific agent names are OK
        assert!(is_dangerous_permission("Agent", "code-review").is_none());
    }

    #[test]
    fn strip_dangerous_rules_filters() {
        use crate::permissions::{PermissionBehavior, PermissionRule};

        let rules = vec![
            PermissionRule {
                tool_name: "Bash".into(),
                pattern: Some("python*".into()),
                behavior: PermissionBehavior::Allow,
            },
            PermissionRule {
                tool_name: "Bash".into(),
                pattern: Some("git*".into()),
                behavior: PermissionBehavior::Allow,
            },
            PermissionRule {
                tool_name: "Bash".into(),
                pattern: Some("rm*".into()),
                behavior: PermissionBehavior::Deny,
            },
        ];

        let (safe, stripped) = strip_dangerous_rules(&rules);
        assert_eq!(safe.len(), 2); // git* (allow) + rm* (deny kept)
        assert_eq!(stripped.len(), 1); // python* stripped
        assert!(stripped[0].contains("python"));
    }
}
