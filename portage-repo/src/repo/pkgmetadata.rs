use std::collections::BTreeMap;

use itertools::Itertools as _;

use crate::error::{Error, Result};

/// Package-level metadata from `metadata.xml`.
///
/// Contains USE flag descriptions extracted from the `<use>` block.
/// The format is defined by the Gentoo
/// [metadata DTD](https://www.gentoo.org/dtd/metadata.dtd)
/// and [GLEP 68](https://www.gentoo.org/glep/glep-0068.html).
///
/// See also [PMS Appendix A](https://projects.gentoo.org/pms/9/pms.html#metadata-xml).
#[derive(Debug, Clone, Default)]
pub struct PkgMetadata {
    /// USE flag descriptions, keyed by flag name, sorted alphabetically.
    ///
    /// Inner XML elements (e.g. `<pkg>`, `<b>`) are flattened to plain text.
    use_flags: BTreeMap<String, String>,
}

impl PkgMetadata {
    /// Parse the contents of a `metadata.xml` file.
    pub fn parse(xml: &str) -> Result<Self> {
        let opts = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        };
        let doc = roxmltree::Document::parse_with_options(xml, opts)
            .map_err(|e| Error::InvalidMetadataXml(e.to_string()))?;
        let root = doc.root_element();
        let mut use_flags = BTreeMap::new();

        for use_node in root.children().filter(|n| n.has_tag_name("use")) {
            for flag_node in use_node.children().filter(|n| n.has_tag_name("flag")) {
                if let Some(name) = flag_node.attribute("name") {
                    use_flags.insert(name.to_string(), collect_text(flag_node));
                }
            }
        }

        Ok(PkgMetadata { use_flags })
    }

    /// USE flag descriptions keyed by flag name, in alphabetical order.
    ///
    /// Inner XML elements (e.g. `<pkg>`, `<b>`) are flattened to plain text.
    pub fn use_flags(&self) -> &BTreeMap<String, String> {
        &self.use_flags
    }

    /// Consume `self` and return the USE flag map.
    pub fn into_use_flags(self) -> BTreeMap<String, String> {
        self.use_flags
    }
}

/// Recursively collect all text content from an XML node, stripping element tags.
///
/// Runs of whitespace (spaces, tabs, newlines) are collapsed to a single space
/// so that multi-line `<flag>` descriptions come out as a clean single line.
fn collect_text(node: roxmltree::Node<'_, '_>) -> String {
    let mut buf = String::new();
    for child in node.children() {
        if child.is_text() {
            if let Some(text) = child.text() {
                buf.push_str(text);
            }
        } else if child.is_element() {
            buf.push_str(&collect_text(child));
        }
    }
    // Collapse all whitespace runs (including newlines) to a single space.
    buf.split_whitespace().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_use_flags() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <use>
    <flag name="foo">Enable foo support</flag>
    <flag name="bar">Enable bar via <pkg>dev-libs/bar</pkg></flag>
  </use>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        assert_eq!(meta.use_flags["foo"], "Enable foo support");
        assert_eq!(meta.use_flags["bar"], "Enable bar via dev-libs/bar");
    }

    #[test]
    fn parse_no_use_block() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <maintainer type="person">
    <email>user@example.com</email>
  </maintainer>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        assert!(meta.use_flags.is_empty());
    }

    #[test]
    fn parse_multiline_description() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE pkgmetadata SYSTEM "https://www.gentoo.org/dtd/metadata.dtd">
<pkgmetadata>
  <use>
    <flag name="dbus">
      Enable dependencies required by glib libraries
      using dbus service to manage settings saving
    </flag>
  </use>
</pkgmetadata>"#;
        let meta = PkgMetadata::parse(xml).unwrap();
        let desc = &meta.use_flags["dbus"];
        assert!(desc.contains("dbus service"));
        // trim() should remove leading/trailing whitespace
        assert!(!desc.starts_with('\n'));
    }

    #[test]
    fn parse_invalid_xml_returns_error() {
        let result = PkgMetadata::parse("<not valid xml");
        assert!(matches!(
            result,
            Err(crate::error::Error::InvalidMetadataXml(_))
        ));
    }
}
