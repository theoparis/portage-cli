# Distfile fetching — reliability (recurring build-killer)

STATUS: **bash root cause FIXED (`8a9558b`); facet B (fetch hardening) open.**

## 0. RO-distdir file not exposed in DISTDIR — FIXED 2026-06-25 (the bash killer)

The real cause of the bash `eapply: patch failed: …/bash53-001` failure: the
patch lives in the host **read-only** distdir `/var/cache/distfiles`, em's fetch
fast-path found it there and returned `AlreadyPresent`, but never linked it into
the writable DISTDIR (`~/.cache/distfiles`) where unpack/eapply look. So em
reported "already present" for a file the build couldn't open — and because the
fetch genuinely succeeded, the fail-fast `bail!` never fired (it surfaced late at
`eapply`). Fixed in `fetch.rs::fetch_distfile`: when the valid copy is in a RO
distdir, symlink it into DISTDIR (hard-link/copy fallback), as portage does.
Unit-tested. Verified: bash builds and **runs in the stage1 chroot**.

## A. Dynamically-built `SRC_URI` extracted as EMPTY (separate; seen once)

NOTE: the `fetch: nothing to fetch (SRC_URI is empty)` seen in the *full* stage1
run did NOT reproduce when building bash alone (em then computed the full 1527-char
SRC_URI correctly, all eclasses inherited, PLEVEL=15) — so this is likely a
phase-sourced/metadata-mode caching artifact (an earlier sourcing left SRC_URI
empty and the fetch reused it via `is_phase_sourced`), not a brush eval bug
(brush computes bash's `${my_urls[*]}` SRC_URI identically to bash). Re-check if
it recurs; otherwise lower priority than thought.

Original notes (kept for the metadata angle):

`app-shells/bash-5.3_p15` builds `SRC_URI` in global scope:

`app-shells/bash-5.3_p15` builds `SRC_URI` in global scope:

```bash
for (( my_patch_idx = 1; my_patch_idx <= PLEVEL; my_patch_idx++ )); do
    printf -v my_patch_ver %s-%03d "${my_p}" "${my_patch_idx}"
    my_urls+=( "mirror://gnu/bash/${MY_P}-patches/${my_patch_ver}" )
    MY_PATCHES+=( "${DISTDIR}/${my_patch_ver}" )
done
SRC_URI="${my_urls[*]} verify-sig? ( ${my_urls[*]/%/.sig} )"
```

em's metadata extraction returns **empty** `SRC_URI` for bash
(`fetch: nothing to fetch (SRC_URI is empty)`), so the `bash53-001..015`
patches are never fetched. The build then dies in `src_prepare`:
`die: eapply: patch failed: …/distfiles/bash53-001` (file absent).

Because SRC_URI is empty, em's fetch **fail-fast is bypassed** — it
"successfully fetches nothing" and proceeds to build, surfacing a *fetch*
problem as a late *prepare* failure. (Contrast: when SRC_URI is correct,
`ebuild.rs:687` `bail!("one or more distfiles could not be fetched")` aborts
before building — what libpcre2 hit.)

Likely a brush/metadata gap evaluating the loop + array-star join
`${my_urls[*]}` in global scope (cf. [[brush-ifs-star-join-fix]] — star-join was
fixed once; re-check `+=( )` array append + `${a[*]}` here). **Verify what
`em` extracts as bash's SRC_URI and where it goes empty** (metadata source vs a
stale/empty cache entry). High value: any ebuild with a computed SRC_URI
(bash, many `-patches` loops, git-snapshot fallbacks) is affected, and the
failure mode is silent-until-prepare.

## B. Genuine fetch failures aren't always recoverable

`dev-libs/libpcre2-10.47` (cross `less`) → `one or more distfiles could not be
fetched`. Here the SRC_URI was correct and em **did** fail-fast — but the file
couldn't be downloaded (mirror/availability/timeout). Recurs often enough to
warrant: more mirrors / `GENTOO_MIRRORS` fallback ordering, retries with
backoff, and resumable/partial-download handling. Confirm em walks
`mirror://` → `GENTOO_MIRRORS` and the upstream URLs in order, and honours
`thirdpartymirrors`.

## Fix direction

1. **Make SRC_URI extraction capture computed values** (facet A) — the metadata
   phase must evaluate global-scope SRC_URI construction (loops, array joins,
   `+=`), not just literal assignments. Add a regression around bash-style
   `${arr[*]}` SRC_URI.
2. **Fetch must fail-fast on *any* required distfile** — already does when the
   file is in the list; the gap is the list being empty/incomplete (facet A).
3. **Harden the fetcher** (facet B) — mirror fallback order, retries, resume.

## Repro
- `em --root /var/tmp/stage1-arm64 --config-root / --oneshot app-shells/bash`
  → `SRC_URI is empty` → `eapply: patch failed: bash53-001`.
- `em --local --cross riscv64-unknown-linux-gnu sys-apps/less` → libpcre2
  `distfiles could not be fetched`.
