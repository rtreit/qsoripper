# GitHub Copilot Instructions

## Documentation and reports

Follow `.github/instructions/documentation.instructions.md` for
documentation, reports, plans, reviews, issue text, pull request text, and
release notes.

Use the root `AGENTS.md` file as the canonical repository guidance for
QsoRipper.

Copilot-specific notes:

- Prefer project skills in `.agents\skills\` for reusable workflows.
- Existing native Copilot CLI skills, agents, prompts, hooks, and instruction
  adapters remain under `.github\`.
- Treat `.codex\` as Codex runtime configuration, not as Copilot source
  material.
- Keep GitHub-specific workflows, adapters, and automation under `.github\`.
- Do not duplicate durable repository rules here. Update `AGENTS.md` instead.
