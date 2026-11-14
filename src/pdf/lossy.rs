//! The lossy image step of a normal PDF: images resampled to the resolution they are shown at, and photos
//! written as JPEG, within floors that hold at every level (see `Lossy`). Off unless asked for.

/// What the lossy image step may do to a normal PDF's images. `Lossy::OFF` changes nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lossy {
    /// A photo shown at more than this many pixels per inch is resampled down to it (never below 150, whatever
    /// is asked); 0: photos are never resampled.
    pub photo_ppi: u16,
    /// Line art (screenshots, charts, diagrams) and scans shown at more than this are resampled down to it
    /// (never below 300, whatever is asked); 0: never.
    pub art_ppi: u16,
    /// JPEG quality on libjpeg's scale (kept within 50 to 95) for a photo that is resampled or stored without
    /// JPEG; 0: no image becomes a JPEG.
    pub jpeg_quality: u8,
    /// Also write an existing JPEG photo again at `jpeg_quality` when that is at least a tenth smaller.
    pub rejpeg: bool,
}

impl Lossy {
    pub const OFF: Lossy = Lossy { photo_ppi: 0, art_ppi: 0, jpeg_quality: 0, rejpeg: false };

    /// The settings of a level: 0 is off; 1, 2 and 3 are what efforts 60, 80 and 100 mean.
    pub fn level(n: u8) -> Lossy {
        match n {
            0 => Lossy::OFF,
            1 => Lossy { photo_ppi: 300, art_ppi: 300, jpeg_quality: 85, rejpeg: false },
            2 => Lossy { photo_ppi: 150, art_ppi: 300, jpeg_quality: 85, rejpeg: false },
            _ => Lossy { photo_ppi: 150, art_ppi: 300, jpeg_quality: 75, rejpeg: true },
        }
    }

    pub fn is_off(&self) -> bool {
        *self == Lossy::OFF
    }
}
