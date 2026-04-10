# TypeScript Multi-Agent Swarm Architecture Analysis

## 1. COORDINATOR MODE ARCHITECTURE

### Entry Point: coordinatorMode.ts
- **isCoordinatorMode()**: Feature gate controlled, checks CLAUDE_CODE_COORDINATOR_MODE env var
- **Purpose**: Orchestration layer where the main agent (coordinator) spawns and manages multiple worker agents
- **Role distinction**: Coordinator never does work directly — delegates to workers
- **System prompt philosophy**: 
  - Coordinator synthesizes findings from workers before delegating implementation
  - Workers are autonomous and execute tasks independently
  - Results flow back as task-notification XML messages

### Key Coordinator Concepts:
- Workers are spawned via **\\\** (the Agent tool)
- Worker results arrive as **\<task-notification>\** XML blocks (system messages with internal structure)
- Coordinator uses **\\\** to continue existing workers
- Coordinator uses **\\\** to abort workers mid-flight

---

## 2. TEAM-BASED MULTI-AGENT SWARM (Agent Swarms Enabled)

### Team Structure (teamHelpers.ts, TeamCreateTool.ts)

**TeamFile Schema** (~/.claude/teams/{teamName}/.team-meta.json):
\\\	ypescript
type TeamFile = {
  name: string                      // Team identifier
  description?: string              // Purpose
  createdAt: number
  leadAgentId: string              // Deterministic: "team-lead@{teamName}"
  leadSessionId?: string            // Leader's actual session UUID
  members: Array<{
    agentId: string                // Format: "name@teamName"
    name: string                   // Agent name (e.g., "researcher", "tester")
    agentType?: string             // Role type for coordination
    model?: string                 // Model override for this member
    joinedAt: number
    tmuxPaneId: string            // Terminal pane ID (if pane-based)
    cwd: string                   // Working directory
    worktreePath?: string         // Git worktree (optional)
    sessionId?: string            // Teammate's session ID (optional)
    subscriptions: string[]       // Events this teammate subscribed to
    backendType?: BackendType     // 'tmux' | 'iterm2' | 'in-process'
    isActive?: boolean            // false when idle, undefined/true when active
    mode?: PermissionMode         // Current permission context
  }>
}
\\\

### Team Creation (TeamCreateTool.ts)
1. Generate unique team name (or use provided)
2. Create deterministic team lead ID: \ormatAgentId(TEAM_LEAD_NAME, teamName)\
3. Initialize TaskList directory for this team (tasks get numbered per team)
4. Write TeamFile with lead member
5. Update AppState.teamContext
6. Register team for session cleanup

### Team Deletion (TeamDeleteTool.ts)
- Only allowed if all non-lead members are inactive (isActive !== true)
- Cleanup team directories and task lists
- Clear color assignments and leader context

---

## 3. AGENT SPAWNING & EXECUTION (runAgent.ts, spawnMultiAgent.ts)

### Agent Types:
- **Built-in agents**: generalPurposeAgent, planAgent, exploreAgent, verificationAgent, etc.
- **Custom agents**: User-defined agents with frontmatter (system prompt, MCP servers, etc.)

### Agent Spawning Modes:

#### A. Coordinator Mode (Single-Process Workers)
- Spawned via **\unAgent()\** in current process
- Each worker gets isolated context
- Results delivered via task-notification XML
- Internal communication via AppState updates

#### B. Team-Based Swarms (Multi-Process)
Two execution backends:

**1. Pane-Based (tmux or iTerm2)**
- Each teammate runs in a terminal pane
- Isolated process (separate Claude Code instance)
- Communication: File-based mailbox system
- Backend abstraction: TmuxBackend / ITermBackend

**2. In-Process (Embedded in Same Process)**
- Teammate runs via AsyncLocalStorage context isolation
- No separate process
- Communication: In-memory + mailbox fallback
- Used when tmux/iTerm2 unavailable
- Type: InProcessBackend

### Spawn Configuration (spawnMultiAgent.ts)

\\\	ypescript
type SpawnTeammateConfig = {
  name: string
  prompt: string
  team_name?: string
  cwd?: string
  use_splitpane?: boolean
  plan_mode_required?: boolean
  model?: string
  agent_type?: string
  description?: string
  invokingRequestId?: string
}
\\\

### CLI Flags Inherited by Teammates:
- Permission mode (bypass, auto, acceptEdits, interactive)
- Model override (--model)
- Settings path (--settings)
- Inline plugins (--plugin-dir)
- Feature flags

---

## 4. AGENT COMMUNICATION ARCHITECTURE

### File-Based Mailbox System (teammateMailbox.ts)

**Structure**: \~/.claude/teams/{teamName}/inboxes/{agentName}.json\

\\\	ypescript
type TeammateMessage = {
  from: string           // Sender agent name
  text: string           // Message content
  timestamp: string      // ISO timestamp
  read: boolean          // Read/unread status
  color?: string         // Sender's assigned color
  summary?: string       // 5-10 word preview
}
\\\

**Operations**:
- **readMailbox(agentName, teamName)**: Read all messages
- **readUnreadMessages(agentName, teamName)**: Read only unread
- **writeToMailbox(agentName, message, teamName)**: Append message
- **Locking**: File-based locks with exponential backoff to handle concurrent writes

### Permission Synchronization (permissionSync.ts)

**Flow**:
1. Worker encounters permission prompt
2. Worker creates permission_request (JSON) with tool info, input, suggestions
3. Worker writes to leader's mailbox as permission_request message
4. Leader's permission poller (useSwarmPermissionPoller.ts) polls mailbox
5. Leader shows permission UI to user
6. Leader sends permission_response back to worker's mailbox
7. Worker's permission poller retrieves response
8. Worker resumes tool execution

**Request Schema**:
\\\	ypescript
type SwarmPermissionRequest = {
  id: string                        // Unique request ID
  workerId: string                  // Worker's agent ID
  workerName: string                // Worker's agent name
  workerColor?: string
  teamName: string
  toolName: string                  // e.g., "Bash", "Edit"
  toolUseId: string
  description: string               // Human-readable summary
  input: Record<string, unknown>   // Tool input
  permissionSuggestions: unknown[]
  status: 'pending' | 'approved' | 'rejected'
  createdAt: number
  resolvedBy?: 'worker' | 'leader'
  resolvedAt?: number
  feedback?: string
  updatedInput?: Record<string, unknown>
  permissionUpdates?: PermissionUpdate[]
}
\\\

### Send Message Tool (SendMessageTool.ts)

**Recipient types**:
- Teammate name (direct message)
- "*" (broadcast to all teammates)
- "uds:<socket-path>" (local Unix domain socket peer)
- "bridge:<session-id>" (Remote Control peer)

**Message types**:
- Plain text (requires summary)
- Structured (shutdown_request, shutdown_response, plan_approval_response)

**Routing**: Messages flow through AppState inbox on leader → task notifications

---

## 5. EXECUTION BACKENDS

### Backend Registry (backends/registry.ts)

**Registry pattern**:
- Cached backend detection (fixed for process lifetime)
- Lazy loading of backend implementations
- Fallback chain: detect best available → fallback to in-process if needed

**Detection flow**:
1. Check if in tmux → use TmuxBackend (native)
2. Check if in iTerm2 → check it2 CLI → use ITermBackend or prompt setup
3. If outside terminal → create external tmux session or fallback to in-process
4. If all fail → in-process mode with fallback flag set

### Backend Interfaces (backends/types.ts)

**PaneBackend** (tmux/iTerm2):
- isAvailable(), isRunningInside()
- createTeammatePaneInSwarmView()
- sendCommandToPane()
- setPaneBorderColor(), setPaneTitle()
- hidePane(), showPane()
- killPane()

**TeammateExecutor** (common interface):
- spawn(config): Launches teammate
- sendMessage(agentId, message): Sends via mailbox
- terminate(agentId, reason): Graceful shutdown request
- kill(agentId): Force termination
- isActive(agentId): Status check

### In-Process Execution (inProcessRunner.ts)

**Flow**:
1. Teammate context via AsyncLocalStorage (runWithTeammateContext)
2. Calls runAgent() with isolated context
3. Agent runs in same process but isolated via context
4. Progress tracked → AppState updates → UI renders
5. On completion: Write idle notification to leader's mailbox
6. On abort: Cleanup and context removal

**Fallback to mailbox**:
- If leader UI queue unavailable → write to file-based mailbox
- Teammates check mailbox for permission responses

---

## 6. TASK MANAGEMENT

### Task Directory Structure

\\\
~/.claude/
  tasks/
    {taskListId}/           # taskListId = sanitizeName(teamName) or sessionId
      {taskNum}.json        # Task state file
      logs/
        {taskNum}.txt       # Task transcript
  teams/
    {teamName}/
      .team-meta.json       # TeamFile
      inboxes/
        {agentName}.json    # Mailbox
      permissions/
        pending/            # Permission requests
        approved/           # Approved history
\\\

### Task Notification Format

Delivered to coordinator as system-role message:
\\\xml
<task-notification>
  <task-id>{agentId}</task-id>
  <status>completed|failed|killed</status>
  <summary>{human-readable status}</summary>
  <result>{agent's final text response}</result>
  <usage>
    <total_tokens>N</total_tokens>
    <tool_uses>N</tool_uses>
    <duration_ms>N</duration_ms>
  </usage>
</task-notification>
\\\

---

## 7. AGENT IDENTITY & CONTEXT

### Agent ID Format
- Coordinator: No agent ID (main session)
- Worker (coordinator mode): Generated UUID
- Team member: "{name}@{teamName}" (deterministic, except team lead)
- Team lead: "team-lead@{teamName}"

### Context Sources (teammate.ts)

**Priority order**:
1. AsyncLocalStorage (in-process teammates) → TeammateContext
2. CLI args (tmux teammates) → dynamicTeamContext
3. Environment variables → CLAUDE_CODE_AGENT_ID, etc.

### Agent Color Assignment

- Each teammate assigned a unique color (red, blue, green, yellow, purple, cyan)
- Stored in TeamFile members[].color
- Used for UI pane borders and message highlighting
- Managed by assignTeammateColor() / clearTeammateColors()

---

## 8. PERMISSION FLOW IN SWARMS

### Worker Perspective (swarmWorkerHandler.ts)

1. Worker tool use encounters permission check
2. Try classifier auto-approval (if available)
3. If denied: Create permission_request
4. Register callback for response (createResolveOnce)
5. Send request to leader via mailbox
6. Show "pending" indicator in AppState
7. Poll mailbox for permission_response
8. On response: Execute callback (onAllow or onReject)
9. Resume tool execution

### Leader Perspective (useSwarmPermissionPoller.ts)

1. Poll worker mailboxes periodically
2. Detect permission_request messages
3. Create permission prompt in UI
4. User approves/rejects
5. Send permission_response back to worker's mailbox
6. Remove from pending

---

## 9. KEY PATTERNS

### 1. Async-Safe Operations
- File-based locks with retries for mailbox access
- AsyncLocalStorage for in-process context isolation
- AbortController for cancellation

### 2. State Management
- AppState holds team context (currentTeam, teammates map)
- TaskList for tracking agent tasks
- Inbox for queued messages

### 3. Graceful Shutdown
- Team lead can request teammate shutdown
- Shutdown messages via mailbox (shutdown_request/response)
- All non-lead members must be idle before team deletion
- Session cleanup registry for team directories

### 4. Environment Inheritance
- CLI flags inherited by teammates
- Model override propagated
- Permission mode (except plan mode required)
- Inline plugins and settings paths

---

## 10. COMPARISON: COORDINATOR vs TEAM SWARMS

| Aspect | Coordinator Mode | Team Swarms |
|--------|------------------|-------------|
| **Spawn** | runAgent() same-process | Pane or in-process |
| **Communication** | AppState updates, task-notification XML | File-based mailbox + XML |
| **Multi-Process** | No | Yes (unless in-process) |
| **Identity** | UUID only | name@teamName |
| **Persistence** | Ephemeral | TeamFile on disk |
| **Scale** | Limited by memory | Scales to multiple processes |
| **Backend** | None (single process) | tmux, iTerm2, or in-process |
| **Permission Sync** | Via AppState | Via mailbox |
| **Task Numbering** | Per session | Per team |

---

## 11. RUST KAMEO ACTOR FRAMEWORK MAPPING

**Recommended mapping for Rust swarm design**:

\\\
TypeScript Concept         → Rust Kameo Actor
─────────────────────────────────────────
Team                       → ActorSystem with named registry
Team Lead                  → Coordinator Actor
Teammate                   → Worker Actor (message handler)
Mailbox                    → Actor's message queue (built-in)
Permission Request         → Message type variant
AppState                   → Shared registry/state
TaskList                   → Actor's internal state
Agent ID (name@team)       → Actor address/path
Synchronized Access        → Actor-based message ordering
Graceful Shutdown          → Supervised restart policy
Backend Detection          → Runtime initialization
\\\

**Key architectural differences**:
- Kameo's actor model **naturally replaces** the file-based mailbox (actors have built-in message queues)
- Permission sync becomes **typed message passing** (no JSON serialization needed)
- Backend abstraction can stay similar (**enum-based dispatch** instead of trait objects)
- Context isolation handled by **separate actor instances** (no AsyncLocalStorage needed)
- Graceful shutdown via **supervisor actors** with defined strategies

