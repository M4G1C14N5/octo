# Tom's patched Octo

Upstream release 2026.11.15 with one patch (MC f001de8d):

- `getNowPlaying` also lists outside (YouTube) songs, from the last now-playing scrobble
  (`submission=false`) per listener and player. An entry goes when that player starts another
  song, or after 10 minutes (sooner once a known song length has run out). The family
  `own_now_playing` filter still applies. Code: `crates/octo/src/controllers/subsonic/now_playing.rs`.

The image is the upstream one with only `/app/octo` replaced. The binary is a release asset
of this fork, built with upstream's toolchain (cargo-chef rust-1.99-bookworm, `--locked`).
It has no built-in AcoustID key (upstream compiles theirs in from a CI secret). Download
verification needs a key of your own, or it stays off; this server does not download.

Rollback: set the Coolify `octo` image back to `ghcr.io/winters27/octo:2026.11.15`.
