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
