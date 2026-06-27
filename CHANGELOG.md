# Changelog

All notable changes to golfsetridak-rust are documented here.
Format: [Keep a Changelog](https://keepachangelog.com/); SemVer.
`deploy.sh` cuts `## [Unreleased]` into a dated version section on each release.

## [Unreleased]

## [0.0.14] - 2026-06-27

## [0.0.13] - 2026-06-27

## [0.0.12] - 2026-06-27

## [0.0.11] - 2026-06-27

## [0.0.10] - 2026-06-27

## [0.0.9] - 2026-06-27

## [0.0.8] - 2026-06-26

## [0.0.7] - 2026-06-26

## [0.0.6] - 2026-06-26

## [0.0.5] - 2026-06-26

## [0.0.4] - 2026-06-26

## [0.0.3] - 2026-06-26

## [0.0.2] - 2026-06-26

### Added
- **Full port of golfsetridak.is** from Next.js + SQLite to pure Rust on the
  AkurAI-Framework (own binary on the akurai-* crates, zero external runtime deps):
  email-OTP auth + sessions + roles, booking engine (single/package/subscription
  + sharing), shop catalog + cart, checkout + Landsbankinn payments + cart
  fulfillment, gift cards (issuance/redemption/scheduled delivery), admin
  (dashboard/bookings/payments/users/settings/templates/announcements),
  `/my` account dashboard, and the static content pages. 99 tests.
- **deploy.sh** release engine — gate → version bump → changelog cut → DB
  snapshot ring (>=5 rollback iterations) → musl build → `akurai-ec2` deploy →
  tag/push → live verify. `staging` + gated `prod` cutover + `rollback-db`.
- Live on **staging** at https://rust.golfsetridak.is.
