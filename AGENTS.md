# AGENTS.md

## Rules

- Never commit, push, or deploy.
- Ask before changes outside the request. Ask questions when design is unclear.
- No comments anywhere (code, toml, nix).
- No unit tests.
- No `static`, `OnceLock`, lazy globals. Pass state explicitly.
- Prefer existing crates over hand-rolled code. Research, check version compat (ratatui 0.30, termwiz 0.23, reqwest 0.13).
- Hand-roll validation only when crate errors are bad (e.g. regex validators).
- Simplest code wins. No pointless wrappers (`main` returns `anyhow::Result` directly).
- Be concise.
- Write your md files in a very short but yet concise way.

## Architecture

- SSH server (russh) rendering ratatui on the backend. UI is trusted: do critical checks server-side anyway.
- Users identified by ssh key only. SSH login name ignored. Unknown keys rejected.
- `syphoria.toml` → `Config` (serde, typesafe) → `AppState::new(config)`.
  - Newtypes, `deny_unknown_fields`, `serde_with` duplicate prevention, cross-checks in `AppState::new`.
- `AppState(&'static AppStateInner)` via `Box::leak`, `Copy`, `Deref`. Restart to reload.
- Stalwart 0.15 is the mail server and source of truth for addresses.
  - `src/stalwart.rs`: management API (provision domains/accounts, add/remove aliases).
  - `src/mail.rs`: JMAP via `jmap-client`, login `user%admin` (master impersonation).
- One Stalwart account per user; addresses are aliases on it; users may only use their own domains.
- `src/ui/`: `Pages` root, `Page` enum via `enum_dispatch`, `View` trait, page stack.
- Async work via `Task` (spawn, poll in render). Never block in render/input.

## Errors

- Modules: `exn::Result<T, ModError>`, `ModError` = thiserror struct describing what this layer does.
- Add context per boundary with `or_raise(|| ...)` / `ok_or_raise`, `bail!`/`ensure!` for new errors. Never flatten into strings.
- Top level (main, config, ui) uses `anyhow`; convert with `exn_anyhow::into_anyhow`, print with `{e:#}`.
- Match API errors before raising (e.g. `Reply::Error(ApiError::FieldAlreadyExists)`), don't downcast.

## Dev

- `devenv up` starts Stalwart: http/jmap 8080, smtp 2525, imap 1143.
- Admin password: `.devenv/state/stalwart/admin-password`.
- Test mail: `printf 'From: a@b.c\r\nTo: x@cool.com\r\nSubject: t\r\n\r\nbody\r\n' | curl -s smtp://127.0.0.1:2525 --mail-from a@b.c --mail-rcpt x@cool.com -T -`
- Don't touch `syphoria.toml` without asking; update `syphoria.example.toml`.

## Verify

- `cargo fmt`, `cargo clippy` clean, then `cargo build` (clippy does not rebuild the binary).
- Test against real Stalwart. Drive TUI via tmux + ssh.
- Clean up test processes and state afterwards.

## Gotchas

- Stalwart `directory.internal.cache.size = "disable"`, else alias changes lag up to 24h.
- Dev Stalwart: `session.auth.require = false`, SPF/DKIM/DMARC off, extra keys in `config.local-keys`.
- `pkill -f <pattern>` kills your own shell if the command contains the pattern.
- Start processes only after files they need are written.
