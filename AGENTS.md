# Agent Instructions

This project uses **br** (Beads Rust) for issue tracking.

## Quick Reference

```bash
br ready              # Find available work
br show <id>          # View issue details
br update <id> --status in_progress  # Claim work
br close <id> --reason "..."  # Complete work
br sync --flush-only  # Export to JSONL for git
```

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

Shell commands like `cp`, `mv`, and `rm` may be aliased to include `-i` (interactive) mode on some systems, causing the agent to hang indefinitely waiting for y/n input.

**Use these forms instead:**
```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**
- `scp` - use `-o BatchMode=yes` for non-interactive
- `ssh` - use `-o BatchMode=yes` to fail instead of prompting
- `apt-get` - use `-y` flag
- `brew` - use `HOMEBREW_NO_AUTO_UPDATE=1` env var

<!-- BEGIN BEADS INTEGRATION -->
## Issue Tracking with br (Beads Rust)

**IMPORTANT**: This project uses **br (Beads Rust)** for ALL issue tracking. Do NOT use markdown TODOs, task lists, or other tracking methods.

### Why br?

- Dependency-aware: Track blockers and relationships between issues
- SQLite + JSONL: Local-first, git-friendly, no daemon needed
- Agent-optimized: JSON output, ready work detection
- Prevents duplicate tracking systems and confusion

### Quick Start

**Check for ready work:**

```bash
br ready --json
```

**Create new issues:**

```bash
br create "Issue title" --description "Detailed context" -t bug -p 1 --json
```

**Update status:**

```bash
br update <id> --status in_progress --json
br update <id> --priority 1 --json
```

**Complete work:**

```bash
br close <id> --reason "Completed" --json
```

### Issue Types

- `bug` - Something broken
- `feature` - New functionality
- `task` - Work item (tests, docs, refactoring)
- `epic` - Large feature with subtasks
- `chore` - Maintenance (dependencies, tooling)

### Priorities

- `0` - Critical (security, data loss, broken builds)
- `1` - High (major features, important bugs)
- `2` - Medium (default, nice-to-have)
- `3` - Low (polish, optimization)
- `4` - Backlog (future ideas)

### Workflow for AI Agents

1. **Check ready work**: `br ready` shows unblocked issues
2. **Claim your task**: `br update <id> --status in_progress`
3. **Work on it**: Implement, test, document
4. **Discover new work?** Create linked issue:
   - `br create "Found bug" --description "Details" -p 1 --json`
   - `br dep add <new-id> <parent-id>`
5. **Complete**: `br close <id> --reason "Done"`

### Sync

br uses explicit sync (never auto-commits to git):

```bash
br sync --flush-only    # Export DB → JSONL
br sync --import-only   # Import JSONL → DB (after git pull)
```

### Important Rules

- ✅ Use br for ALL task tracking
- ✅ Always use `--json` flag for programmatic use
- ✅ Check `br ready` before asking "what should I work on?"
- ❌ Do NOT create markdown TODO lists
- ❌ Do NOT use external issue trackers
- ❌ Do NOT duplicate tracking systems

<!-- END BEADS INTEGRATION -->

## Landing the Plane (Session Completion)

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   br sync --flush-only
   git add .beads/
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
