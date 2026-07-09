# portage-binpkg

Gentoo binary package (GPKG) reading and writing per
[GLEP 78](https://www.gentoo.org/glep/glep-0078.html).

Used by the [`em`](https://github.com/lu-zero/portage-cli) Portage CLI for
`-b`/`--buildpkg`, `-k`/`--usepkg`, and `-g`/`--getbinpkg`.

## Features

- **GPKG writer** — [`write_gpkg`] packs an installed image into a GPKG container
  (GNU `tar` + `zstd`, matching Portage's approach for capabilities/ACLs)
- **Metadata reader** — [`read_metadata`] reads GPKG metadata without full extraction
- **Image extraction** — [`extract_image`] unpacks the installed image from a GPKG

## Example

```rust
use portage_binpkg::{GpkgInput, write_gpkg};

write_gpkg(&GpkgInput {
    image_dir: "/path/to/image",
    output_path: "/path/to/pkg.tbz2",
    // ...
})?;
```

[`write_gpkg`]: https://docs.rs/portage-binpkg/latest/portage_binpkg/fn.write_gpkg.html
[`read_metadata`]: https://docs.rs/portage-binpkg/latest/portage_binpkg/fn.read_metadata.html
[`extract_image`]: https://docs.rs/portage-binpkg/latest/portage_binpkg/fn.extract_image.html

## License

MIT