use criterion::{Criterion, black_box, criterion_group, criterion_main};
use pkgcraft::dep::DependencySet;
use pkgcraft::eapi::EAPI_LATEST_OFFICIAL;
use portage_atom::DepEntry;

const TEXLIVE_RDEPEND: &str = "\
>=app-text/texlive-core-2023-r1\n\
app-text/psutils\n\
>=app-text/texlive-fontutils-2023-r1\n\
media-gfx/sam2p\n\
texi2html? ( app-text/texi2html )\n\
sys-apps/texinfo\n\
app-text/t1utils\n\
>=app-text/lcdf-typetools-2.92[kpathsea]\n\
truetype? ( >=app-text/ttf2pk2-2.0_p20230311 )\n\
app-text/ps2eps\n\
png? ( app-text/dvipng )\n\
X? ( >=app-text/xdvik-22.87 )\n\
>=app-text/texlive-basic-2023-r1\n\
>=app-text/texlive-fontsrecommended-2023-r1\n\
>=app-text/texlive-latex-2023-r1\n\
luatex? ( >=app-text/texlive-luatex-2023-r1 )\n\
>=app-text/texlive-latexrecommended-2023-r1\n\
metapost? ( >=app-text/texlive-metapost-2023-r1 )\n\
>=app-text/texlive-plaingeneric-2023-r1\n\
pdfannotextractor? ( dev-tex/pdfannotextractor )\n\
extra? ( >=app-text/texlive-bibtexextra-2023-r1 >=app-text/texlive-binextra-2023-r1 >=app-text/texlive-fontsextra-2023-r1 >=app-text/texlive-formatsextra-2023-r1 >=app-text/texlive-latexextra-2023-r1 )\n\
xetex? ( >=app-text/texlive-xetex-2023-r1 )\n\
graphics? ( >=app-text/texlive-pictures-2023-r1 )\n\
science? ( >=app-text/texlive-mathscience-2023-r1 )\n\
publishers? ( >=app-text/texlive-publishers-2023-r1 )\n\
music? ( >=app-text/texlive-music-2023-r1 )\n\
pstricks? ( >=app-text/texlive-pstricks-2023-r1 )\n\
context? ( >=app-text/texlive-context-2023-r1 )\n\
games? ( >=app-text/texlive-games-2023-r1 )\n\
humanities? ( >=app-text/texlive-humanities-2023-r1 )\n\
tex4ht? ( >=dev-tex/tex4ht-20230311_p69739 )\n\
xml? ( >=app-text/texlive-formatsextra-2023-r1 )\n\
l10n_af? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_ar? ( >=app-text/texlive-langarabic-2023-r1 )\n\
l10n_fa? ( >=app-text/texlive-langarabic-2023-r1 )\n\
l10n_hy? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
cjk? ( >=app-text/texlive-langcjk-2023-r1 )\n\
l10n_hr? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_bg? ( >=app-text/texlive-langcyrillic-2023-r1 )\n\
l10n_br? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_ru? ( >=app-text/texlive-langcyrillic-2023-r1 )\n\
l10n_uk? ( >=app-text/texlive-langcyrillic-2023-r1 )\n\
l10n_cs? ( >=app-text/texlive-langczechslovak-2023-r1 )\n\
l10n_sk? ( >=app-text/texlive-langczechslovak-2023-r1 )\n\
l10n_da? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_nl? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_en? ( >=app-text/texlive-langenglish-2023-r1 )\n\
l10n_fi? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_eu? ( >=app-text/texlive-langfrench-2023-r1 )\n\
l10n_fr? ( >=app-text/texlive-langfrench-2023-r1 )\n\
l10n_de? ( >=app-text/texlive-langgerman-2023-r1 )\n\
l10n_el? ( >=app-text/texlive-langgreek-2023-r1 )\n\
l10n_he? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_hu? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_as? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_bn? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_gu? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_hi? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_kn? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_ml? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_mr? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_or? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_pa? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_sa? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_ta? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_te? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_it? ( >=app-text/texlive-langitalian-2023-r1 )\n\
l10n_ja? ( >=app-text/texlive-langjapanese-2023-r1 )\n\
l10n_ko? ( >=app-text/texlive-langkorean-2023-r1 )\n\
l10n_la? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_lt? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_lv? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_mn? ( >=app-text/texlive-langcyrillic-2023-r1 )\n\
l10n_nb? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_nn? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_no? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_cy? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_eo? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_et? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_ga? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_rm? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_hsb? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_ia? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_id? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_is? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_lo? ( >=app-text/texlive-langother-2023-r1 )\n\
l10n_ro? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_sq? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_sr? ( >=app-text/texlive-langeuropean-2023-r1 >=app-text/texlive-langcyrillic-2023-r1 )\n\
l10n_sl? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_tr? ( >=app-text/texlive-langeuropean-2023-r1 )\n\
l10n_pl? ( >=app-text/texlive-langpolish-2023-r1 )";

const PANDOC_RDEPEND: &str = "\
>=dev-haskell/aeson-0.7\n\
>=dev-haskell/aeson-pretty-0.8.9\n\
>=dev-haskell/attoparsec-0.12\n\
>=dev-haskell/base64-bytestring-0.1\n\
>=dev-haskell/blaze-html-0.9\n\
>=dev-haskell/blaze-markup-0.8\n\
>=dev-haskell/case-insensitive-1.2\n\
>=dev-haskell/citeproc-0.7\n\
>=dev-haskell/commonmark-0.2.2\n\
>=dev-haskell/commonmark-extensions-0.2.3.1\n\
>=dev-haskell/commonmark-pandoc-0.2.1.2\n\
>=dev-haskell/connection-0.3.1\n\
>=dev-haskell/data-default-0.4\n\
>=dev-haskell/doclayout-0.4\n\
>=dev-haskell/doctemplates-0.10\n\
>=dev-haskell/emojis-0.1\n\
>=dev-haskell/file-embed-0.0\n\
>=dev-haskell/glob-0.7\n\
>=dev-haskell/haddock-library-1.10\n\
>=dev-haskell/hslua-module-doclayout-1.0.4\n\
>=dev-haskell/hslua-module-path-1.0\n\
>=dev-haskell/hslua-module-system-1.0\n\
>=dev-haskell/hslua-module-text-1.0\n\
>=dev-haskell/hslua-module-version-1.0\n\
>=dev-haskell/http-client-0.4.30\n\
>=dev-haskell/http-client-tls-0.2.4\n\
>=dev-haskell/http-types-0.8\n\
>=dev-haskell/ipynb-0.2\n\
>=dev-haskell/jira-wiki-markup-1.4\n\
>=dev-haskell/juicypixels-3.1.6.1\n\
>=dev-haskell/lpeg-1.0.1\n\
>=dev-haskell/network-2.6\n\
>=dev-haskell/network-uri-2.6\n\
>=dev-haskell/pandoc-lua-marshal-0.1.5\n\
>=dev-haskell/pandoc-types-1.22.2\n\
>=dev-haskell/pretty-show-1.10\n\
>=dev-haskell/random-1\n\
>=dev-haskell/safe-0.3.18\n\
>=dev-haskell/scientific-0.3\n\
>=dev-haskell/sha-1.6\n\
>=dev-haskell/skylighting-0.12.3.1\n\
>=dev-haskell/skylighting-core-0.12.3.1\n\
>=dev-haskell/split-0.2\n\
>=dev-haskell/syb-0.1\n\
>=dev-haskell/tagsoup-0.14.6\n\
>=dev-haskell/temporary-1.1\n\
>=dev-haskell/texmath-0.12.5\n\
>=dev-haskell/text-conversions-0.3\n\
>=dev-haskell/unicode-collation-0.1.1\n\
>=dev-haskell/unicode-transforms-0.3\n\
>=dev-haskell/xml-1.3.12\n\
>=dev-haskell/xml-conduit-1.9.1.1\n\
>=dev-haskell/xml-types-0.3\n\
>=dev-haskell/yaml-0.11\n\
>=dev-haskell/zip-archive-0.2.3.4\n\
>=dev-haskell/zlib-0.5\n\
>=dev-lang/ghc-8.10.1\n\
>=dev-haskell/hslua-2.2\n\
trypandoc? ( >=dev-haskell/wai-0.3 >=dev-haskell/wai-extra-3.0.24 )";

const FFMPEG_RDEPEND: &str = "\
virtual/libiconv\n\
X? ( x11-libs/libX11 x11-libs/libXext x11-libs/libXv x11-libs/libxcb )\n\
alsa? ( media-libs/alsa-lib )\n\
amr? ( media-libs/opencore-amr )\n\
amrenc? ( media-libs/vo-amrwbenc )\n\
bluray? ( media-libs/libbluray )\n\
bs2b? ( media-libs/libbs2b )\n\
bzip2? ( app-arch/bzip2 )\n\
cdio? ( dev-libs/libcdio-paranoia )\n\
chromaprint? ( media-libs/chromaprint )\n\
codec2? ( media-libs/codec2 )\n\
dav1d? ( media-libs/dav1d )\n\
drm? ( x11-libs/libdrm )\n\
fdk? ( media-libs/fdk-aac )\n\
flite? ( app-accessibility/flite )\n\
fontconfig? ( media-libs/fontconfig )\n\
frei0r? ( media-plugins/frei0r-plugins )\n\
fribidi? ( dev-libs/fribidi )\n\
gcrypt? ( dev-libs/libgcrypt )\n\
gme? ( media-libs/game-music-emu )\n\
gmp? ( dev-libs/gmp )\n\
gnutls? ( !openssl? ( net-libs/gnutls ) )\n\
gsm? ( media-sound/gsm )\n\
iec61883? ( media-libs/libiec61883 sys-libs/libavc1394 sys-libs/libraw1394 )\n\
ieee1394? ( media-libs/libdc1394 sys-libs/libraw1394 )\n\
jack? ( virtual/jack )\n\
jpeg2k? ( media-libs/openjpeg )\n\
kvazaar? ( media-libs/kvazaar )\n\
lame? ( media-sound/lame )\n\
libaom? ( media-libs/libaom )\n\
libaribb24? ( media-libs/aribb24 )\n\
libass? ( media-libs/libass )\n\
libcaca? ( media-libs/libcaca )\n\
libilbc? ( media-libs/libilbc )\n\
librtmp? ( media-video/rtmpdump )\n\
libsoxr? ( media-libs/soxr )\n\
lv2? ( media-libs/lilv media-libs/lv2 )\n\
lzma? ( app-arch/xz-utils )\n\
modplug? ( media-libs/libmodplug )\n\
ocr? ( app-text/tesseract )\n\
openal? ( media-libs/openal )\n\
opencl? ( virtual/opencl )\n\
opengl? ( media-libs/libglvnd )\n\
openh264? ( media-libs/openh264 )\n\
openmpt? ( media-libs/libopenmpt )\n\
openssl? ( >=dev-libs/openssl-3 )\n\
opus? ( media-libs/opus )\n\
pulseaudio? ( media-libs/libpulse )\n\
rabbitmq? ( net-libs/rabbitmq-c )\n\
rav1e? ( >=media-video/rav1e-0.4 )\n\
rubberband? ( media-libs/rubberband )\n\
samba? ( net-fs/samba )\n\
sdl? ( media-libs/libsdl2 )\n\
snappy? ( app-arch/snappy )\n\
sndio? ( media-sound/sndio )\n\
speex? ( media-libs/speex )\n\
srt? ( net-libs/srt )\n\
ssh? ( net-libs/libssh )\n\
svg? ( dev-libs/glib >=gnome-base/librsvg-2.52 x11-libs/cairo )\n\
svt-av1? ( >=media-libs/svt-av1-0.8.4 )\n\
theora? ( media-libs/libtheora )\n\
truetype? ( media-libs/freetype )\n\
twolame? ( media-sound/twolame )\n\
v4l? ( media-libs/libv4l )\n\
vaapi? ( media-libs/libva )\n\
vdpau? ( x11-libs/libX11 x11-libs/libvdpau )\n\
vidstab? ( media-libs/vidstab )\n\
vorbis? ( media-libs/libvorbis )\n\
vpx? ( media-libs/libvpx )\n\
vulkan? ( media-libs/vulkan-loader )\n\
webp? ( media-libs/libwebp )\n\
x264? ( media-libs/x264 )\n\
x265? ( media-libs/x265 )\n\
xml? ( dev-libs/libxml2 )\n\
xvid? ( media-libs/xvid )";

fn bench_realworld(c: &mut Criterion) {
    let eapi = *EAPI_LATEST_OFFICIAL;

    let mut group = c.benchmark_group("realworld/depset");

    group.bench_function("portage-atom: texlive", |b| {
        b.iter(|| black_box(DepEntry::parse(TEXLIVE_RDEPEND).unwrap()))
    });
    group.bench_function("pkgcraft: texlive", |b| {
        b.iter(|| black_box(DependencySet::package(TEXLIVE_RDEPEND, eapi).unwrap()))
    });

    group.bench_function("portage-atom: pandoc", |b| {
        b.iter(|| black_box(DepEntry::parse(PANDOC_RDEPEND).unwrap()))
    });
    group.bench_function("pkgcraft: pandoc", |b| {
        b.iter(|| black_box(DependencySet::package(PANDOC_RDEPEND, eapi).unwrap()))
    });

    group.bench_function("portage-atom: ffmpeg", |b| {
        b.iter(|| black_box(DepEntry::parse(FFMPEG_RDEPEND).unwrap()))
    });
    group.bench_function("pkgcraft: ffmpeg", |b| {
        b.iter(|| black_box(DependencySet::package(FFMPEG_RDEPEND, eapi).unwrap()))
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default().sample_size(100);
    targets = bench_realworld
);

criterion_main!(benches);
