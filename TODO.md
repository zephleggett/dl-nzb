# TODO

- [x] Parse yEnc `=ypart` headers so segment offsets are correct on disk
      (resolves files arriving with zero-filled gaps that PAR2 couldn't repair).
- [x] Discard NNTP connections that hit a mid-pipeline read error so the next
      worker doesn't read stale bytes.
- [x] Pre-warm the connection pool so the first second of a download already
      uses every connection.
- [x] Per-segment retry instead of per-batch retry. (Now genuinely per-article:
      the article-granularity engine retries only the failed article and keeps
      already-received segments; was previously per-50-segment-batch.)
- [x] CRC32 verification of each decoded segment (per `=yend pcrc32=`).
- [x] Write to `*.partial` and rename only on completion.
- [x] Suppress decorative human-readable output in `--json` mode.
- [x] Health probe uses `DATE` instead of `NOOP` (some providers reject NOOP).
- [x] Mock-NNTP integration test that exercises full pipeline (offsets, retries,
      missing segments).
- [x] Article-granularity download engine (flume MPMC + continuous sliding
      window); no idle connections, bounded tail.
- [x] PAR2-on-demand: defer recovery volumes, fetch only when data is
      missing/corrupt (`post_processing.download_all_par2` to opt out).
- [x] PAR2-based filename deobfuscation (16k-hash match) before repair.
- [x] STAT-all pre-flight availability + repairability estimate (all modes).
- [x] Ctrl-C cancels the whole process (skip post-processing; 2nd = force quit;
      cancellable par2 repair / rar extraction).
- [ ] Verify `unrar`/`unrar_sys` redistribution terms for public release.
- [ ] Decide/update `--force` semantics now that resume is removed.
- [ ] RELEASE: publish par2-rs v0.3.0 and switch `Cargo.toml` from
      `path = "../par2-rs"` back to `tag = "v0.3.0"`; bump dl-nzb version.
- [ ] Optional: par2-on-demand "smallest-vol-first, minimal" incremental fetch
      (currently fetches all deferred recovery once any data segment fails).
- [ ] Harden NNTP reads against a malicious/broken server that never sends a
      newline: bound `read_response`/`read_article_body` growth during the read
      (currently size-checked only after the line completes; bounded by the
      60-120s read timeout, so low risk).
