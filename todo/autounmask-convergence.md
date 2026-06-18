# `--autounmask-write` must converge in a single invocation

## Bug (2026-06-18)

`em --autounmask --autounmask-write` only discovers **one layer** of required
config changes per run. On a minimal profile (e.g. a fresh stage3) resolving
`firefox` needs several rounds: pass 1 writes `media-libs/freetype harfbuzz`,
`x11-libs/cairo X`, `media-video/ffmpeg` license, …; a *second* `em` invocation
then surfaces the next set unlocked by those, and so on. We had to loop
`em --autounmask-write` until `rc=0` to get a full plan — that external loop is a
workaround, not the intended behaviour.

emerge does this internally: one `emerge --autounmask-write --autounmask-backtrack=y`
call backtracks until the autounmask set is closed, writing all changes at once
(or reporting the full set in-memory for `-p`).

> Caveat (2026-06-18): the original observation may be partly a `-p` artifact —
> `emerge -p --autounmask-write` does **not** write (pretend suppresses the write),
> so a `-p` loop never converges. emerge *without* `-p` converged in a single
> write-pass on a fresh stage3. Whether `em` shares this `-p`-suppresses-write
> behaviour — i.e. whether em's looping was the same artifact rather than a real
> single-layer limit — is **not yet re-verified**; confirm before treating this as
> a confirmed em bug.

## Expected

A single `em --autounmask[-write]` should iterate its own solve→collect-changes
loop to a fixpoint and emit/write the **complete** set of USE / license /
keyword changes (and the resulting full plan), with `rc=0` when the closure is
satisfiable. No caller-side looping.

## Where

`portage-cli/src/query/depgraph/autounmask.rs` (+ the solve loop in
`query/depgraph/mod.rs`). Today autounmask changes are collected from a single
solved graph; they need to be fed back into config and re-solved until no new
change appears (bounded, like the existing upgrade fixpoint in `resolve_targets`).

## Repro

```bash
# in a fresh stage3 chroot (minimal profile), as root:
em -e --autounmask --autounmask-write -p www-client/firefox   # rc=1, partial plan
em -e --autounmask --autounmask-write -p www-client/firefox   # rc=1, more config
# ...repeats until rc=0 — should have been one call
```

Surfaced while validating emptytree toolchain behaviour in a stage3 chroot
(see [[em-emptytree]] — the `dev-lang/rust` slot-22 investigation).
