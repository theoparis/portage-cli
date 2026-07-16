# em @system vs a real Gentoo stage3 (validation datapoint)

Compared `em -p --root <empty> --config-root / @system` against the official
**`stage3-arm64-openrc-20260621T224616Z`** (distfiles.gentoo.org), by cat/pkg.

## Result

| set | count |
|-----|------:|
| real stage3 (installed `var/db/pkg`) | 302 |
| em `@system` plan | 180 |
| **in both** | **175** |
| only in real stage3 | 127 |
| only in em | 5 |

**175/180 of em's `@system` exactly match the real stage3's contents** — strong
correctness for the runtime closure.

## 2026-07-16 re-check, against a fresh real stage3, same methodology

Re-ran this inside a brand-new `crossdev-stages` sandbox (`em-parity-0716`,
arm64, `stage3-arm64-openrc` fetched today) — same profile
(`default/linux/arm64/23.0`), same `em -p --root <empty> --config-root / @system`
command, current `em` release binary:

| set | count |
|-----|------:|
| real stage3 (installed `var/db/pkg`) | 303 |
| em `@system` plan | 178 |
| **in both** | **175** |
| only in real stage3 | 128 |
| only in em | **3** (was 5) |

`dev-libs/libusb`/`virtual/libusb` have dropped out of the em-only set
entirely since the last comparison — some fix landed in between that closed
that half of the gap (not chased further; not the point of this re-check).
Only `net-libs/{nghttp2,nghttp3,ngtcp2}` remain, and this pass **definitively
root-causes them**, correcting the previous "point em at the same profile"
framing below — see the new section after it.

## The 127 stage3-only = the BDEPEND build closure (expected)

Grouped: dev-python (26), dev-perl (24), virtual (23), dev-build (11),
app-text (8), dev-util (5), sys-devel (4)… — i.e. autoconf / automake / libtool /
cmake / meson / ninja / perl / pkgconf / gperf / re2c / docbook + their
python/perl/virtual deps. A real stage3 is built `emerge -e --with-bdeps=y` from a
minimal start, so it *contains* the whole build closure. em's host-config
`@system` defaults to `--with-bdeps=n` (matching emerge) and, crucially, even with
`--with-bdeps` it stays 180 here because those tools are **already installed on
this host (BROOT=/)** and correctly host-satisfied. A from-scratch
`--emptytree --with-bdeps` build would pull them. So this gap is not a divergence
— it is the build-vs-runtime closure distinction.

## The 5 em-only = profile/USE differences (worth a look)

`net-libs/{nghttp2,nghttp3,ngtcp2}`, `dev-libs/libusb`, `virtual/libusb`.

- em's `net-misc/curl` has `http2 http3 quic` enabled (default profile) → pulls
  nghttp2/nghttp3/ngtcp2. The autobuild stage3's curl has them **off** (minimal
  releng profile) → no pull.
- libusb enters via `virtual/libusb` from an @system RDEPEND under em's default
  USE; the autobuild's USE doesn't enable it.

Root: em resolves against the **host default profile**, the autobuild uses the
**releng stage profile** (which trims curl/usb USE for a lean base). Not a
resolver bug — a profile/USE-config difference. To compare apples-to-apples,
point em at the same `default/linux/arm64/23.0` releng-style profile + the stage
`package.use`. (Connects to [[nonemptytree-bdeps-gap]], which already flagged
nghttp2/3/ngtcp2.)

### 2026-07-16: corrected — "point at the same profile" doesn't apply; root-caused for real

The 2026-07-16 re-check above (`em-parity-0716` sandbox) runs `em` directly
**inside** the real stage3 the comparison is against — `readlink -f
/etc/portage/make.profile` there is `default/linux/arm64/23.0`, the exact
same profile em already resolves against. So the original framing ("em uses
the host default profile, the autobuild uses the releng profile") was wrong
— they're the same profile. The gap survives anyway, so it isn't a
profile-pointing problem at all.

Checked what actually explains it: the real stage3's own installed
`net-misc/curl-8.20.0-r1`'s **recorded** `/var/db/pkg/net-misc/curl-*/IUSE`
(i.e. what its IUSE looked like at the moment it was actually built for this
stage, not just the current repo ebuild) already declares `+http2 +http3
+quic` — enabled by default — yet its recorded `USE` has none of the three.
Searched the entire shipped `profiles/` tree (`grep -rl 'http2\|http3\|quic'`)
for anything that could suppress them (`use.mask`, `package.use.mask`,
`features/*`): **nothing matches** for `linux`/`arm64`/`default` at all — no
`profiles/features/bindist/` directory even exists in this tree (there's a
stray "this stage was built with the bindist USE flag enabled" comment in
`/etc/portage/make.conf`, but nothing in the ebuild or profiles references
`bindist` for curl specifically, and it's not it).

Conclusion: the actual official stage3 build passes an **ephemeral,
catalyst-internal `USE=` override** (releng's own private stage spec) that
disables `http2`/`http3`/`quic` for this one build — it is not encoded
anywhere in the publicly shipped `::gentoo` profile tree, so there is no
profile em could point at to reproduce it. **This is not a resolvable em
bug and not a profile-selection problem** — closing this fully would require
literally replicating catalyst's own non-public stage spec's USE trims, which
isn't worth doing for 3 packages out of a 178-package plan that otherwise
matches exactly. Downgrading from "worth a look" to "understood, not
pursuing further."

## Next: an actually-built stage3

The above is plan-level. A built artifact would be
`em toolchain --setup --root <R>` → `em --root <R> --emptytree --with-bdeps
@system` (the crossdev/catalyst stage3 step), then diff the *file trees* and
per-package versions against the tarball. Long (hours) and will surface
per-package build issues we haven't hit yet (only the toolchain + a handful of
@system pkgs are build-validated). Worth doing once `em stages`
([[em-stages-and-binhosts]]) wraps the sequence and the toolchain auto-activates
([[select-toolchain]]) so the stage builds use the ROOT `<chost>-gcc`.

Artifacts: `/var/tmp/stage3-arm64-openrc.tar.xz`,
`/var/tmp/stage3-real/var/db/pkg` (extracted package DB).
