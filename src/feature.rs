//! The parts of the engine a build may leave out, one Cargo feature each (`Cargo.toml` lists them), and which
//! of them this build has. A build without a part does not compile its code: a call that cannot work without
//! it returns [`crate::Error::NeedsFeature`], and a step that needs it is skipped.

/// A part of the engine that a build may leave out, named as its Cargo feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    /// The gpress archive: `compress` and `expand`.
    Archive,
    /// Normal PDF: JPEG images get optimal Huffman tables.
    Jpeg,
    /// CCITT fax images: written as JBIG2 in a normal PDF, stored as their pixels in the archive.
    Fax,
    /// Encrypted PDFs the empty password opens: rewritten with the same encryption (normal PDF), stored
    /// decrypted (archive).
    Encryption,
    /// The existing library (Lepton) for JPEG streams in the archive.
    Existing,
}

impl Feature {
    /// Every feature, in the order of their bits.
    pub const ALL: [Feature; 5] =
        [Feature::Archive, Feature::Jpeg, Feature::Fax, Feature::Encryption, Feature::Existing];

    /// The Cargo feature's name.
    pub fn name(self) -> &'static str {
        match self {
            Feature::Archive => "archive",
            Feature::Jpeg => "jpeg",
            Feature::Fax => "fax",
            Feature::Encryption => "encryption",
            Feature::Existing => "existing",
        }
    }

    /// The feature from its Cargo name.
    pub fn from_name(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }

    /// One bit per feature, stable across versions (for the C and WebAssembly layers): archive 1, jpeg 2,
    /// fax 4, encryption 8, existing 16.
    pub fn bit(self) -> u32 {
        1 << Feature::ALL.iter().position(|&f| f == self).unwrap_or(0)
    }

    /// The feature of a bit (exactly one set), as `bit` gives it.
    pub fn from_bit(bit: u32) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.bit() == bit)
    }

    /// Whether this build has the feature.
    pub fn enabled(self) -> bool {
        match self {
            Feature::Archive => cfg!(feature = "archive"),
            Feature::Jpeg => cfg!(feature = "jpeg"),
            Feature::Fax => cfg!(feature = "fax"),
            Feature::Encryption => cfg!(feature = "encryption"),
            Feature::Existing => cfg!(feature = "existing"),
        }
    }

    /// What the feature does, for messages.
    fn what(self) -> &'static str {
        match self {
            Feature::Archive => "the gpress archive (compress and expand)",
            Feature::Jpeg => "the JPEG Huffman-table step",
            Feature::Fax => "the CCITT fax and JBIG2 codecs",
            Feature::Encryption => "encrypted PDFs (decrypting and encrypting again)",
            Feature::Existing => "the existing library (Lepton)",
        }
    }

    /// Why something was not done: this build leaves the feature out.
    pub fn missing(self) -> String {
        format!("this build leaves out {}; it needs the `{}` feature", self.what(), self.name())
    }
}

impl std::fmt::Display for Feature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The features this build has, as bits (see [`Feature::bit`]).
pub fn enabled_bits() -> u32 {
    Feature::ALL.iter().filter(|f| f.enabled()).map(|f| f.bit()).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_bits_are_stable_and_go_both_ways() {
        let names: Vec<&str> = Feature::ALL.iter().map(|f| f.name()).collect();
        assert_eq!(names, ["archive", "jpeg", "fax", "encryption", "existing"]);
        let bits: Vec<u32> = Feature::ALL.iter().map(|f| f.bit()).collect();
        assert_eq!(bits, [1, 2, 4, 8, 16]);
        for f in Feature::ALL {
            assert_eq!(Feature::from_name(f.name()), Some(f));
            assert_eq!(Feature::from_bit(f.bit()), Some(f));
            assert_eq!(f.to_string(), f.name());
            assert!(f.missing().contains(&format!("`{}`", f.name())));
        }
        assert_eq!((Feature::from_name("web"), Feature::from_bit(0), Feature::from_bit(3)), (None, None, None));
        // Lepton is only used by the archive, so a build with it has the archive too.
        assert!(!Feature::Existing.enabled() || Feature::Archive.enabled());
        assert_eq!(enabled_bits() & Feature::Jpeg.bit() != 0, cfg!(feature = "jpeg"));
    }
}
