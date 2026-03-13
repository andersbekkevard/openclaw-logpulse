# Beads Rust (br) — Issue Tracking

This repository uses **br** (Beads Rust) for local-first issue tracking.

**Learn more:** [github.com/Dicklesworthstone/beads_rust](https://github.com/Dicklesworthstone/beads_rust)

## Quick Start

```bash
br ready              # See what's actionable
br list               # View all issues
br show <issue-id>    # View issue details
br update <id> --status in_progress  # Claim work
br close <id> --reason "Done"        # Complete work
br sync --flush-only  # Export to JSONL for git commit
```

## Architecture

```
.beads/
├── beads.db        # SQLite database (primary storage)
├── issues.jsonl    # JSONL export (for git)
├── config.yaml     # Project configuration
└── metadata.json   # Workspace metadata
```

*Beads Rust: Agent-first issue tracking, SQLite + JSONL* ⚡
