# Vendored AkurAI-Framework (fork)

This directory is a **self-contained fork** of the AkurAI-Framework crates this app
depends on. The app builds from these vendored copies — it does **not** path-depend on
the live `../AkurAI-Framework` checkout. This is the bootstrap model: like
`create-next-app` / `npm create svelte`, you get a standalone app pinned to a framework
version, immune to whatever in-progress work is happening in the upstream framework repo.

## Pinned version

- **Source repo:** `olibuijr/AkurAI-Framework`
- **Forked at commit:** `f347126`
- **Vendored on:** 2026-06-27
- **Crates vendored (12):** http, json, template, markdown, router, css, storage, ws,
  collections, blobs, vector, toml (the transitive closure of this app's direct deps)

## Re-vendoring (deliberate framework update)

Updating the framework is an explicit action, never automatic. From the repo root:

```sh
FW=~/Projects/AkurAI-Framework
REF=<commit-ish>                       # e.g. a tag or HEAD
rm -rf framework && mkdir -p framework
git -C "$FW" archive "$REF" -- \
  crates/http crates/json crates/template crates/markdown crates/router crates/css \
  crates/storage crates/ws crates/collections crates/blobs crates/vector crates/toml \
  | tar -x -C framework/
# then update the "Forked at commit" line above, rebuild, and run the gate.
```

If the app starts using a new framework crate, add it (and its `../`-relative
sibling deps) to the archive list above and re-vendor.
