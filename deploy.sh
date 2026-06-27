#!/usr/bin/env bash
# deploy.sh — golfsetridak-rust release engine.
#
#   ./deploy.sh [patch|minor|major]      staging release (default patch):
#                                        gate → bump version → cut changelog →
#                                        BACKUP db → build → deploy → tag/push → verify
#   ./deploy.sh prod [patch|minor|major] THE CUTOVER — replace the live Next.js
#                                        golfsetridak.is (gated: GOLF_CUTOVER=yes)
#   ./deploy.sh rollback-db [<ts>]       restore a DB snapshot (newest, or <ts>) + restart
#   ./deploy.sh list-backups             list the DB snapshot ring on the server
#
# ALWAYS deploy through this script and verify on the LIVE url — never localhost.
# Keeps the last KEEP_BACKUPS (>=5) DB snapshots on the server for rollback.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"; cd "$ROOT"
NAME="golfsetridak-rust"
PORT=8110
REPO="olibuijr/golfsetridak-rust"
SSH="akurai-mail"
APPDATA="/opt/$NAME/app/data"
BACKUP_DIR="/var/backups/$NAME"
KEEP_BACKUPS=7                       # keep >=5 rollback iterations
MUSL="target/x86_64-unknown-linux-musl/release/golfsetridak"
say(){ printf '\033[1;34m▸ %s\033[0m\n' "$*"; }
die(){ printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }

# ── DB snapshot ring (server-side) ──────────────────────────────────────────
# Stops the service for a consistent copy of the akurai-storage B+tree files,
# snapshots them to a timestamped dir, prunes to the newest $KEEP_BACKUPS, and
# (callers restart via deploy or rollback). Safe to run when no data yet.
backup_db(){
  local ts; ts="$(date +%Y%m%d-%H%M%S)"
  say "Backup: DB snapshot $ts (keeping newest $KEEP_BACKUPS)"
  ssh "$SSH" "sudo mkdir -p '$BACKUP_DIR'
    if [ -d '$APPDATA' ]; then
      sudo systemctl stop $NAME.service 2>/dev/null || true
      sudo cp -a '$APPDATA' '$BACKUP_DIR/db-$ts'
      sudo systemctl start $NAME.service 2>/dev/null || true
      # prune to newest $KEEP_BACKUPS
      ls -1dt $BACKUP_DIR/db-* 2>/dev/null | tail -n +\$(( $KEEP_BACKUPS + 1 )) | xargs -r sudo rm -rf
      echo \"snapshots: \$(ls -1d $BACKUP_DIR/db-* 2>/dev/null | wc -l)\"
    else echo '(no data dir yet — nothing to back up)'; fi"
}
list_backups(){ ssh "$SSH" "ls -1dt $BACKUP_DIR/db-* 2>/dev/null || echo '(none)'"; }
rollback_db(){
  local ts="${1:-}"
  local src
  if [ -n "$ts" ]; then src="$BACKUP_DIR/db-$ts"; else src="\$(ls -1dt $BACKUP_DIR/db-* 2>/dev/null | head -1)"; fi
  say "Rollback DB ← ${ts:-newest}"
  ssh "$SSH" "set -e; SRC=$src
    [ -d \"\$SRC\" ] || { echo \"no snapshot \$SRC\"; exit 1; }
    sudo systemctl stop $NAME.service
    sudo cp -a '$APPDATA' '$APPDATA.pre-rollback-\$(date +%s)' 2>/dev/null || true
    sudo rm -rf '$APPDATA'; sudo cp -a \"\$SRC\" '$APPDATA'
    sudo systemctl start $NAME.service && echo 'rolled back + restarted'"
  say "DB restored. Verify: https://rust.golfsetridak.is"
}

case "${1:-}" in
  list-backups) list_backups; exit 0 ;;
  rollback-db)  rollback_db "${2:-}"; exit 0 ;;
esac

# ── Mode + bump ─────────────────────────────────────────────────────────────
MODE=staging; BUMP=patch; DOMAIN="rust.golfsetridak.is"
if [ "${1:-}" = "prod" ]; then
  MODE=prod; DOMAIN="golfsetridak.is"; BUMP="${2:-patch}"
  [ "${GOLF_CUTOVER:-}" = "yes" ] || die "PROD CUTOVER gated — replaces the LIVE site. Verify migrated data on staging first, then: GOLF_CUTOVER=yes ./deploy.sh prod"
else
  BUMP="${1:-patch}"
fi
case "$BUMP" in patch|minor|major) ;; *) die "usage: $0 [patch|minor|major] | prod [bump] | rollback-db [ts] | list-backups" ;; esac

# ── Gate ────────────────────────────────────────────────────────────────────
say "Gate: fmt / clippy / test"
cargo fmt --all -- --check || die "rustfmt (run: cargo fmt --all)"
cargo clippy --all-targets -- -D warnings || die "clippy"
cargo test || die "tests"

# ── Version bump + changelog cut ────────────────────────────────────────────
CUR="$(tr -d '[:space:]' < VERSION)"; IFS='.' read -r MA MI PA <<< "$CUR"
case "$BUMP" in major) MA=$((MA+1));MI=0;PA=0;; minor) MI=$((MI+1));PA=0;; patch) PA=$((PA+1));; esac
NEW="$MA.$MI.$PA"; DATE="$(date +%Y-%m-%d)"
say "Version: $CUR → $NEW ($BUMP)"
printf '%s\n' "$NEW" > VERSION
sed -i -E "s/^version = \"$CUR\"/version = \"$NEW\"/" Cargo.toml
if [ -f CHANGELOG.md ]; then
  awk -v vh="## [$NEW] - $DATE" '$0=="## [Unreleased]"{print;print"";print vh;next}{print}' CHANGELOG.md > CHANGELOG.md.tmp && mv CHANGELOG.md.tmp CHANGELOG.md
fi

# ── Backup DB, build, deploy ────────────────────────────────────────────────
backup_db
say "Build: static musl binary"
rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 || true
cargo build --release --target x86_64-unknown-linux-musl -q || die "musl build"
say "Stage app dir"
STAGE="$ROOT/.stage-deploy"; rm -rf "$STAGE"; mkdir -p "$STAGE"
cp -r frontend backend content "$STAGE/"; rm -rf "$STAGE/backend/data" "$STAGE/data"
command -v akurai-ec2 >/dev/null || die "akurai-ec2 not on PATH"
say "Deploy: $NAME on 127.0.0.1:$PORT → $DOMAIN"
akurai-ec2 deploy-binary "$NAME" "$MUSL" "$PORT" "$STAGE"
say "Embeddings: configure Router-backed semantic search"
ssh "$SSH" "sudo bash -s" <<'EMBED'
set -euo pipefail
mkdir -p /etc/golfsetridak-rust /etc/systemd/system/golfsetridak-rust.service.d
ROUTER_KEY="$(awk -F= '/^AKURAI_ROUTER_API_KEY=/{print substr($0, index($0, "=") + 1)}' /etc/akurai-router/router.env 2>/dev/null || true)"
cat > /etc/golfsetridak-rust/env <<EOF
AKURAI_EMBED_URL=http://127.0.0.1:4219/v1/embeddings
AKURAI_EMBED_MODEL=intfloat/multilingual-e5-small
AKURAI_EMBED_API_KEY=$ROUTER_KEY
EOF
chmod 0600 /etc/golfsetridak-rust/env
cat > /etc/systemd/system/golfsetridak-rust.service.d/10-embeddings.conf <<EOF
[Service]
EnvironmentFile=/etc/golfsetridak-rust/env
EOF
systemctl daemon-reload
systemctl restart golfsetridak-rust.service
EMBED
akurai-ec2 nginx-proxy "$DOMAIN" "$PORT"
akurai-ec2 tls "$DOMAIN" || say "  TLS deferred — run 'akurai-ec2 tls $DOMAIN' once DNS resolves"

# ── Commit, tag, push ───────────────────────────────────────────────────────
say "Git: commit + tag v$NEW"
git add -A; git commit -q -m "release: v$NEW ($MODE)"; git tag -a "v$NEW" -m "v$NEW"
git push -u origin HEAD; git push origin "v$NEW"

# ── Verify on the LIVE url (from the box — reliable, not the dev host) ──────
code=$(ssh "$SSH" "curl -s -o /dev/null -w '%{http_code}' https://$DOMAIN/login" 2>/dev/null || echo 000)
say "Verify (live): https://$DOMAIN/login → $code"
say "Released v$NEW → https://$DOMAIN  (DB snapshots: rollback with ./deploy.sh rollback-db)"
[ "$MODE" = "prod" ] && say "CUTOVER done. Next app retained for rollback — see DEPLOY.md."
