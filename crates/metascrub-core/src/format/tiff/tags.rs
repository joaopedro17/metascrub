//! What each TIFF and DNG tag is for: the allowlist of what pictures need,
//! and how the rest is reported.

use crate::model::FindingKind;

pub(super) const STRIP_OFFSETS: u16 = 273;
pub(super) const STRIP_BYTE_COUNTS: u16 = 279;
pub(super) const TILE_OFFSETS: u16 = 324;
pub(super) const TILE_BYTE_COUNTS: u16 = 325;
pub(super) const NEW_SUBFILE_TYPE: u16 = 254;
pub(super) const COMPRESSION: u16 = 259;
pub(super) const PHOTOMETRIC: u16 = 262;
pub(super) const JPEG_TABLES: u16 = 347;
pub(super) const DNG_VERSION: u16 = 50706;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// Needed to show the picture, and says nothing about anyone: kept
    /// without a finding.
    Structure,
    /// Where the strips or tiles are; their sizes are another tag.
    PictureData,
    SubImages,
    Exif,
    Gps,
    Interoperability,
    Orientation,
    ColorProfile,
    Xmp,
    Iptc,
    Photoshop,
    /// Photoshop's layered document, saved next to the flat picture.
    Layers,
    /// Where a map or aerial picture lies on Earth (GeoTIFF).
    Placement,
    /// DNG's camera model, which raw editors need to pick a color profile.
    CameraModel,
    /// Removed, with a name and kind worth reporting on its own.
    Named(&'static str, FindingKind),
    /// Removed, and reported the way EXIF is elsewhere.
    Other,
}

pub(super) fn role(tag: u16) -> Role {
    use FindingKind::*;
    match tag {
        STRIP_OFFSETS | TILE_OFFSETS => Role::PictureData,
        330 => Role::SubImages,
        34665 => Role::Exif,
        34853 => Role::Gps,
        40965 => Role::Interoperability,
        274 => Role::Orientation,
        34675 => Role::ColorProfile,
        700 => Role::Xmp,
        33723 => Role::Iptc,
        34377 => Role::Photoshop,
        37724 => Role::Layers,
        33550 | 33922 | 34264 | 34735..=34737 => Role::Placement,
        50708 => Role::CameraModel,
        269 => Role::Named("DocumentName", FilePath),
        285 => Role::Named("PageName", Description),
        337 => Role::Named("TargetPrinter", DeviceModel),
        50709 => Role::Named("LocalizedCameraModel", DeviceModel),
        50735 => Role::Named("CameraSerialNumber", DeviceSerial),
        50736 => Role::Named("LensInfo", DeviceModel),
        50740 => Role::Named("DNGPrivateData", UnknownMetadata),
        50781 => Role::Named("RawDataUniqueID", DocumentId),
        50827 => Role::Named("OriginalRawFileName", FileName),
        50828 => Role::Named("OriginalRawFileData", EmbeddedFile),
        50898 | 50899 => Role::Named("Title", Description),
        50966 => Role::Named("PreviewApplicationName", Software),
        50967 => Role::Named("PreviewApplicationVersion", Software),
        50968 => Role::Named("PreviewSettingsName", Description),
        50971 => Role::Named("PreviewDateTime", Timestamp),
        51043 => Role::Named("TimeCodes", Timestamp),
        51081 => Role::Named("ReelName", Description),
        51105 => Role::Named("CameraLabel", Description),
        tag if is_structure(tag) => Role::Structure,
        _ => Role::Other,
    }
}

fn is_structure(tag: u16) -> bool {
    matches!(
        tag,
        // TIFF 6.0 baseline and extensions: size, samples, compression,
        // colors, resolution, layout.
        254..=259
            | 262..=266
            | 277..=284
            | 286
            | 287
            | 290..=293
            | 296
            | 297
            | 301
            | 317..=323
            | 325
            | 332..=334
            | 336
            | 338..=342
            | 346
            | 347
            | 529..=532
            | 559
            // TIFF/EP: the color filter pattern and the standard's version.
            | 33421
            | 33422
            | 37398
            // GDAL: which value means "no data".
            | 42113
            // DNG: versions, color filter, levels, scaling, crops, color
            // matrices and profiles, opcodes, digests of the pixels, depth
            // maps and masks, JPEG XL settings.
            | 50706
            | 50707
            | 50710..=50734
            | 50737..=50739
            | 50752
            | 50778..=50780
            | 50829..=50834
            | 50879
            | 50931
            | 50932
            | 50934..=50942
            | 50964
            | 50965
            | 50970
            | 50972
            | 50974
            | 50975
            | 50981
            | 50982
            | 51008
            | 51009
            | 51022
            | 51041
            | 51044
            | 51058
            | 51089..=51091
            | 51107..=51112
            | 51125
            | 51177..=51182
            | 52525
            | 52526
            | 52528..=52539
            | 52543
            | 52544
            | 52547
            | 52550..=52555
    )
}

/// Compressions whose data holds only the picture. JPEG (7, and 34892 for
/// lossy DNG) and JPEG XL (52546) are checked stream by stream.
pub(super) fn known_compression(compression: u64) -> bool {
    matches!(
        compression,
        1 | 2 | 3 | 4 | 5 | 7 | 8 | 32773 | 32946 | 34892 | 50000 | 52546
    )
}

pub(super) fn is_jpeg(compression: u64) -> bool {
    matches!(compression, 7 | 34892)
}

/// Color models a TIFF picture can use. Raw sensor data (color filter
/// array, linear raw), depth and masks are only read in DNG, whose
/// specification says how; other raw formats keep what decoders need in
/// their maker notes.
pub(super) fn known_photometric(photometric: u64, dng: bool) -> bool {
    match photometric {
        0..=6 | 8..=10 => true,
        32803 | 34892 | 51177 | 52527 => dng,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn privacy_tags_are_never_structure() {
        for tag in [
            269, 270, 271, 272, 305, 306, 315, 316, 33432, 50709, 50735, 50740, 50781, 50827,
            50828, 50966, 50971,
        ] {
            assert_ne!(role(tag), Role::Structure, "tag {tag}");
        }
        assert_eq!(role(256), Role::Structure);
        assert_eq!(role(50721), Role::Structure);
        assert_eq!(role(50708), Role::CameraModel);
    }
}
