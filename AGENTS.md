# AGENTS.md

Documentation for this repo lives in the **akurai-notes** MCP — not in this repo.

- **Canonical note:** `golfsetridak-rust — Docs` (note **2**)
- **Index:** `AkurAI EC2 — Documentation Index` (note **19**)
- Retrieve: `search_notes("golfsetridak-rust")` or `get_note(2)`

Secrets live in the **akurai-passvault** MCP (never in this repo).

> `CLAUDE.md` intentionally contains only `@AGENTS.md`.
> Note: `framework/` is a vendored fork of AkurAI-Framework — see the docs note.

## Brevo CLI & Skills (installed 2026-07-04)

| Tool | Type | Install | Usage |
|------|------|---------|-------|
| `@getbrevo/cli` v2.0.0 | Official CLI | `bun install -g @getbrevo/cli` | `brevo login`, `brevo app *` — OAuth app management |
| `brevo-cli` skill | Claude Code skill | `brevo skill:cli install` (auto-refresh) | `~/.claude/skills/brevo-cli/SKILL.md` |
| Brevo Automation | Claude skill | ClawHub `brevo-automation` | `~/.claude/skills/brevo-automation/` — MCP via Composio |
| Membrane Brevo | Agent skill | `bunx skills add membranedev/application-skills --skill brevo` | `~/.agents/skills/brevo/` — Membrane CLI auth |
| CLI-Anything Hub | Meta skill | `bunx skills add HKUDS/CLI-Anything --skill cli-hub-meta-skill` | Agent-driven CLI discovery for any software |

### Brevo DNS (domains authenticated)
- **golfsetridak.is** → Authenticated. Zone: `/etc/akurai-dns/zones/golfsetridak.is.toml` on EC2. Records: Brevo code TXT, DKIM1/DKIM2 CNAME, DMARC TXT, SPF includes `spf.brevo.com`.
- **olibuijr.com** → Pending propagation via 1984.is (`1984dns`). Records set in panel (entry IDs: 6282209, 6303849, 6303854, 6303859).

### Email delivery architecture
- **Sidecar:** `golfsetridak-sidecar` (Python, port `3002`, systemd) — HTTP-to-Brevo-SMTP relay
- **Flow:** Rust binary → `POST /email` to `127.0.0.1:3002` (sidecar) → Brevo SMTP `smtp-relay.brevo.com:587` (STARTTLS)
- **Config:** `SIDECAR_PORT=3002` in `/etc/golfsetridak-rust/env`; Brevo SMTP creds in akurai-passvault entry `Brevo SMTP Relay`
- **Sidecar source:** `/opt/golfsetridak-sidecar/sidecar.py` on EC2; systemd unit `golfsetridak-sidecar.service`
- **Fallback:** If `SIDECAR_PORT` is unset, the app logs OTPs to stdout (`LogDeliver` mode)

### DNS tools reference
- `akurai-ec2` — EC2 box ops (SSH `akurai-mail`, deploy, nginx, TLS, ports)
- `1984dns` — 1984.is FreeDNS management for `olibuijr.com` and other 1984-hosted zones
- `akurai-dns` — Authoritative DNS on EC2 for `golfsetridak.is` (zone files in `/etc/akurai-dns/zones/`)
