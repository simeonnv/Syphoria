# AGENTS.md

## Rules

- Never commit, push, or deploy.
- Ask before changes outside the request. Ask questions when design is unclear.
- No comments anywhere (code, toml, nix).
- No unit tests.
- No `static`, `OnceLock`, lazy globals. Pass state explicitly.
- Prefer existing crates over hand-rolled code. Research, check version compat (ratatui 0.30, termwiz 0.23, reqwest 0.13).
- Hand-roll validation only when crate errors are bad (e.g. regex validators).
- Simplest code wins. No pointless wrappers (`main` returns `exn::Result<(), MainError>` directly).
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

- All error types live in `src/error.rs`, by situation.
- Each error has only the fields it needs. Kinds are per type and say what the caller can do.
  - `StorageError { kind: StorageKind, message }`, temporary when `Locked`.
  - `RemoteError { kind: RemoteKind, status, message }`, `status` = `Temporary` if a retry may help.
  - Message only: `ConfigError`, `InputError` (validation), `SshError`, `UiError`, `MainError`. `HandlerError` wraps `Exn<SshError>` for russh.
- Return `exn::Result<T, E>`. Cross situations with `or_raise(|| ...)` / `ok_or_raise`; new errors via `bail!`/`ensure!`/`.raise()`. Never flatten into strings.
- Same situation: plain `?`, or `or_raise` to add context. jmap errors: `RemoteError::jmap`.
- Context names what failed at that call; callers add the rest. One context per call, no shared `let err = || ...` closures.
- Never drop errors (`let _ =`, `.ok()`, `map_err(|_| ..)`, `Err(_) =>`). Propagate; if cleanup also fails keep both as children (`IteratorExt::raise`). Exception: non-fatal teardown (closing channels).
- Machines: outer frames' kind may not match the cause, so walk the tree (`is_temporary`). Top-frame `kind` only where the error is raised (e.g. `AlreadyExists` in `Stalwart::create`).
- Humans: `{e:?}` prints the tree (logs, main); UI shows one line via `ui::failure` (`chain` + retry hint).

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
