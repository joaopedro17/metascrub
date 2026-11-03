//! Archives as desktop tools write them, with the details they leak, and
//! the unsafe ones MetaScrub must refuse or contain.

use std::io::{Cursor, Write};

use flate2::{Compression, GzBuilder};
use tar::{Builder, EntryType, Header};
use zip::write::{FullFileOptions, SimpleFileOptions};
use zip::{CompressionMethod, DateTime, ZipWriter};

pub struct Files {
    pub photos_zip: Vec<u8>,
    pub photos_tar: Vec<u8>,
    pub photos_tar_gz: Vec<u8>,
    pub bomb_zip: Vec<u8>,
    pub traversal_zip: Vec<u8>,
    pub encrypted_zip: Vec<u8>,
    pub deep_zip: Vec<u8>,
    pub traversal_tar: Vec<u8>,
    pub app_jar: Vec<u8>,
}

/// 2026-03-14 09:26:52 UTC.
const DATE: u64 = 1_773_480_412;

/// The Unix owner extra field ("ux"): version 1, user and group 1000.
const OWNER_FIELD: [u8; 11] = [1, 4, 0xE8, 3, 0, 0, 4, 0xE8, 3, 0, 0];

const APPLE_DOUBLE: &[u8] = b"\0\x05\x16\x07\0\x02\0\0Mac OS X        ";

pub fn files(photo: &[u8], document: &[u8], png: &[u8]) -> Files {
    let inner = simple_zip(&[("screenshot.png", png)]);
    let photos_tar = photos_tar(photo, document);
    Files {
        photos_zip: photos_zip(photo, document, &inner),
        photos_tar_gz: gzip(&photos_tar, "photos.tar"),
        photos_tar,
        bomb_zip: simple_zip(&[("zeros.bin", &vec![0; 8 * 1024 * 1024])]),
        traversal_zip: simple_zip(&[("../../evil.txt", b"outside")]),
        encrypted_zip: encrypted(&simple_zip(&[("secret.txt", b"secret")])),
        deep_zip: (0..5).fold(simple_zip(&[("camera.jpg", photo)]), |data, _| {
            simple_zip(&[("nested.zip", &data)])
        }),
        traversal_tar: traversal_tar(),
        app_jar: simple_zip(&[
            ("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n"),
            ("App.class", b"\xCA\xFE\xBA\xBE"),
        ]),
    }
}

fn dated() -> DateTime {
    DateTime::from_date_and_time(2026, 3, 14, 9, 26, 52).unwrap()
}

fn simple_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let options = SimpleFileOptions::default().last_modified_time(dated());
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (name, data) in entries {
        zip.start_file(*name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn photos_zip(photo: &[u8], document: &[u8], inner: &[u8]) -> Vec<u8> {
    let mut options = FullFileOptions::default()
        .last_modified_time(dated())
        .unix_permissions(0o600);
    options
        .add_extra_field(0x7875, OWNER_FIELD.to_vec().into_boxed_slice(), false)
        .unwrap();
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.set_comment("Holiday photos, sent by Jane Doe").unwrap();
    zip.add_directory("Holiday/", options.clone()).unwrap();
    for (name, data, method) in [
        (
            "Holiday/IMG_20260314_092652.jpg",
            photo,
            CompressionMethod::Stored,
        ),
        (
            "Holiday/itinerary.docx",
            document,
            CompressionMethod::Deflated,
        ),
        (
            "Holiday/notes.txt",
            b"Remember the charger.\n".as_slice(),
            CompressionMethod::Deflated,
        ),
        ("Holiday/screens.zip", inner, CompressionMethod::Stored),
        (
            "Holiday/.DS_Store",
            b"\0\0\0\x01Bud1".as_slice(),
            CompressionMethod::Deflated,
        ),
        (
            "__MACOSX/Holiday/._IMG_20260314_092652.jpg",
            APPLE_DOUBLE,
            CompressionMethod::Deflated,
        ),
    ] {
        let options = options.clone().compression_method(method);
        let options = if name.ends_with("notes.txt") {
            options.with_file_comment("draft from Jane")
        } else {
            options
        };
        zip.start_file(name, options).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.start_file("Holiday/upload.sh", options.clone().unix_permissions(0o700))
        .unwrap();
    zip.write_all(b"#!/bin/sh\necho uploading\n").unwrap();
    zip.add_symlink("Holiday/latest.jpg", "IMG_20260314_092652.jpg", options)
        .unwrap();
    zip.finish().unwrap().into_inner()
}

/// `<length> key=value\n`, where the length counts itself.
fn pax_record(key: &str, value: &str) -> Vec<u8> {
    let rest = key.len() + value.len() + 3;
    let mut length = rest + 1;
    while length != rest + length.to_string().len() {
        length = rest + length.to_string().len();
    }
    format!("{length} {key}={value}\n").into_bytes()
}

fn header(kind: EntryType, size: u64, mode: u32) -> Header {
    let mut header = Header::new_ustar();
    header.set_entry_type(kind);
    header.set_size(size);
    header.set_mode(mode);
    header.set_mtime(DATE);
    header.set_uid(1000);
    header.set_gid(1000);
    header.set_username("jane").unwrap();
    header.set_groupname("staff").unwrap();
    header
}

fn photos_tar(photo: &[u8], document: &[u8]) -> Vec<u8> {
    let mut tar = Builder::new(Vec::new());
    tar.append_data(
        &mut header(EntryType::Directory, 0, 0o755),
        "Holiday/",
        &b""[..],
    )
    .unwrap();
    // What GNU tar --xattrs saves for a file the browser downloaded.
    let records = [
        pax_record(
            "SCHILY.xattr.user.xdg.origin.url",
            "https://photos.example.com/jane/IMG_20260314_092652.jpg",
        ),
        pax_record("atime", "1773480412.5"),
    ]
    .concat();
    let mut pax = header(EntryType::XHeader, records.len() as u64, 0o644);
    tar.append_data(
        &mut pax,
        "PaxHeaders/IMG_20260314_092652.jpg",
        records.as_slice(),
    )
    .unwrap();
    for (name, data, mode) in [
        ("Holiday/IMG_20260314_092652.jpg", photo, 0o644),
        ("Holiday/itinerary.docx", document, 0o644),
        (
            "Holiday/notes.txt",
            b"Remember the charger.\n".as_slice(),
            0o600,
        ),
        (
            "Holiday/upload.sh",
            b"#!/bin/sh\necho uploading\n".as_slice(),
            0o700,
        ),
        ("Holiday/._notes.txt", APPLE_DOUBLE, 0o644),
    ] {
        tar.append_data(
            &mut header(EntryType::Regular, data.len() as u64, mode),
            name,
            data,
        )
        .unwrap();
    }
    tar.append_link(
        &mut header(EntryType::Symlink, 0, 0o777),
        "Holiday/latest.jpg",
        "IMG_20260314_092652.jpg",
    )
    .unwrap();
    tar.into_inner().unwrap()
}

fn gzip(data: &[u8], name: &str) -> Vec<u8> {
    let mut encoder = GzBuilder::new()
        .filename(name)
        .mtime(DATE as u32)
        .write(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// The "encrypted" flag set in the local and central headers: enough to be
/// recognized, without a real cipher.
fn encrypted(zip: &[u8]) -> Vec<u8> {
    let mut data = zip.to_vec();
    for (signature, flags) in [(&b"PK\x03\x04"[..], 6), (b"PK\x01\x02", 8)] {
        let at = data.windows(4).position(|w| w == signature).unwrap();
        data[at + flags] |= 1;
    }
    data
}

/// The TAR writer refuses `..`, so the name is written into the header by hand.
fn traversal_tar() -> Vec<u8> {
    let mut header = header(EntryType::Regular, 7, 0o644);
    let name = b"../../evil.txt";
    header.as_old_mut().name[..name.len()].copy_from_slice(name);
    header.set_cksum();
    let mut tar = Builder::new(Vec::new());
    tar.append(&header, &b"outside"[..]).unwrap();
    tar.into_inner().unwrap()
}
