# Deploy & Cutover — golfsetridak-rust

> Always deploy via `./deploy.sh` and verify on the **live URL** — never localhost.

## Status (2026-06-26)
- **Feature-complete** port of golfsetridak.is (Next.js+SQLite → pure Rust on AkurAI-Framework). 99 tests, gate-clean.
- **Live on STAGING:** https://rust.golfsetridak.is (port 8110 on the EC2 box `mail.olibuijr.com` / 3.94.46.219, behind nginx + Let's Encrypt). HTTP→HTTPS 301 works. systemd unit `golfsetridak-rust.service`.
- **NOT cut over.** Production golfsetridak.is still runs the Next.js app.

## Architecture recap
- App = single static **musl** binary serving its own app dir: `serve --dir <app>/frontend` with `backend/` + `content/` as siblings. Data in **akurai-storage B+trees** under the working dir's `data/` (`/opt/golfsetridak-rust/app/data/...`, gitignored — never ship it).
- **No outbound TLS** in the app (framework rule) → it POSTs plaintext to the **sidecar** (`golfsetridak-sidecar`, repo) on `127.0.0.1:$SIDECAR_PORT` for email/SMS/payments. Sidecar does the real HTTPS.
- Framework crates are **path deps** to `../AkurAI-Framework/crates/*` (currently 0.8.x, under active dev — rebuild + retest after pulling it).

## Deploy (staging) — always via deploy.sh, verify on the live URL
```
./deploy.sh [patch|minor|major]   # gate → bump VERSION+Cargo.toml → cut CHANGELOG →
                                  # BACKUP db → musl build → akurai-ec2 deploy → tag/push → verify live
```
Every release bumps the version + cuts `CHANGELOG.md` and tags `vX.Y.Z`. Port **8110** (8094–8099 are taken: framework/idp/akurai-platform/vpn). DNS `rust.golfsetridak.is` is an A record in the akurai-dns `golfsetridak.is` zone on the box (`/etc/akurai-dns/zones/golfsetridak.is.toml`, reload `systemctl reload akurai-dns`, bump SOA serial).

## DB backups & rollback (≥5 iterations kept)
Each deploy snapshots the akurai-storage B+tree data dir (`/opt/golfsetridak-rust/app/data`) to `/var/backups/golfsetridak-rust/db-<ts>` on the box, stopping the service for a consistent copy, and prunes to the newest 7.
```
./deploy.sh list-backups          # list the snapshot ring
./deploy.sh rollback-db [<ts>]    # restore newest (or a specific) snapshot + restart
```
Binary rollback: redeploy the previous git tag (`git checkout vX.Y.Z && ./deploy.sh patch`) or restart the retained Next app for a full prod fallback.

## Still TODO before a real cutover
1. **Deploy + run the sidecar** alongside the app (it's built: `golfsetridak-sidecar`, repo). Set the app's `$SIDECAR_PORT` to reach it, and the sidecar's env: **`SMTP_HOST=mail.olibuijr.com`** (its default mirrors the dead `mail.hbhf.is`), `SMS_TO_*`, `LANDSBANKINN_*`, `PAYDAY_*`. Without it, OTP/SMS/payments run in mock mode.
2. **Migrate real data:** `VACUUM INTO` a read-only snapshot of the live SQLite (`/opt/golfsetrid/data/golfsetrid.sqlite`), run `golfsetridak-migrate --from <snapshot> --to <datadir>` (repo `golfsetridak-migrate`), point the staging app at that datadir, and verify the real 69 users / 388 bookings / 116 payments render + book correctly. **This is the cutover gate** — staging currently has an empty DB.
3. **Verify end-to-end on staging** with migrated data: login (real OTP via sidecar→SES), a booking, a checkout (Landsbankinn via sidecar), admin.

## Cutover (PROD) — gated
```
GOLF_CUTOVER=yes ./deploy.sh prod   # refuses without the env flag
```
Only after (1)-(3) verified AND Ólafur's explicit go. The cutover points `golfsetridak.is` at the Rust app and disables the Next.js service — **keep the Next app (`golfsetrid.service`) for instant rollback** (`systemctl start golfsetrid` + repoint nginx). See memory `golfsetridak-runtime-deploy` for the current Next deploy.
