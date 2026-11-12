use std::io::Cursor;

use super::*;

/// An element with an 8-byte size (or an unknown one).
fn element(id: u32, data: &[u8], unknown: bool) -> Vec<u8> {
    let mut out: Vec<u8> = id
        .to_be_bytes()
        .iter()
        .copied()
        .skip_while(|&b| b == 0)
        .collect();
    if unknown {
        out.extend([0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    } else {
        out.push(0x01);
        out.extend(&(data.len() as u64).to_be_bytes()[1..]);
    }
    out.extend(data);
    out
}

fn el(id: u32, data: &[u8]) -> Vec<u8> {
    element(id, data, false)
}

fn header() -> Vec<u8> {
    el(EBML, &el(DOC_TYPE, b"webm"))
}

fn info(crc: bool) -> Vec<u8> {
    let mut children = Vec::new();
    if crc {
        children.extend(el(CRC32, &[1, 2, 3, 4]));
    }
    children.extend(el(0x2AD7B1, &[0x0F, 0x42, 0x40]));
    children.extend(el(0x7BA9, b"Jane's video"));
    children.extend(el(0x4D80, b"Recorder 2.1"));
    children.extend(el(0x5741, b"Recorder 2.1"));
    el(INFO, &children)
}

fn tracks() -> Vec<u8> {
    let entry = [
        el(0xD7, &[1]),
        el(0x83, &[1]),
        el(0x86, b"V_VP8"),
        el(0x536E, b"Jane's webcam"),
    ]
    .concat();
    el(TRACKS, &el(TRACK_ENTRY, &entry))
}

fn cluster(unknown: bool) -> Vec<u8> {
    let blocks = [el(0xE7, &[0]), el(0xA3, &[0x81, 0, 0, 0x80, 0xAB, 0xCD])].concat();
    element(CLUSTER, &blocks, unknown)
}

fn tags() -> Vec<u8> {
    let simple = [el(0x45A3, b"ARTIST"), el(0x4487, b"Jane Doe")].concat();
    el(TAGS, &el(0x7373, &el(0x67C8, &simple)))
}

/// A seek entry pointing `distance` bytes into the segment's data.
fn seek(distance: u64) -> Vec<u8> {
    el(
        SEEK,
        &[
            el(0x53AB, &[0x12, 0x54, 0xC3, 0x67]),
            el(SEEK_POSITION, &distance.to_be_bytes()),
        ]
        .concat(),
    )
}

/// A WebM with an index, info, a track, a cluster and tags, then `after`.
fn webm(unknown: bool, crc: bool, after: &[u8]) -> Vec<u8> {
    // The index's own size is fixed, so where the tags are is known first.
    let index_len = el(SEEK_HEAD, &seek(0)).len() as u64;
    let body_before_tags = [info(crc), tracks(), cluster(unknown)].concat();
    let index = el(SEEK_HEAD, &seek(index_len + body_before_tags.len() as u64));
    let body = [index, body_before_tags, tags()].concat();
    [header(), element(SEGMENT, &body, unknown), after.to_vec()].concat()
}

fn run(data: &[u8]) -> Result<(Vec<u8>, Vec<Finding>), FormatError> {
    let options = Options::default();
    let mut out = Vec::new();
    let findings = Matroska.clean(&mut Cursor::new(data), &mut out, &options)?;
    assert_eq!(
        Matroska.inspect(&mut Cursor::new(data), &options)?,
        findings,
        "inspect and clean disagree"
    );
    Ok((out, findings))
}

fn assert_clean(out: &[u8]) {
    let (again, findings) = run(out).unwrap();
    assert_eq!(again, out, "cleaning twice changed the output");
    assert!(
        findings
            .iter()
            .all(|f| f.disposition.is_acceptable_after_cleaning()),
        "{findings:?}"
    );
}

fn has(data: &[u8], text: &[u8]) -> bool {
    data.windows(text.len()).any(|w| w == text)
}

#[test]
fn metadata_goes_and_the_video_stays() {
    for unknown in [false, true] {
        let data = webm(unknown, false, b"");
        let (out, findings) = run(&data).unwrap();
        let kinds: Vec<_> = findings.iter().map(|f| f.kind).collect();
        assert_eq!(
            kinds,
            [
                FindingKind::Description,
                FindingKind::Software,
                FindingKind::Software,
                FindingKind::Description,
                FindingKind::PersonName,
            ],
            "unknown sizes: {unknown}"
        );
        assert_eq!(out.len(), data.len());
        for gone in [&b"Jane"[..], b"Recorder"] {
            assert!(!has(&out, gone), "{}", String::from_utf8_lossy(gone));
        }
        // The cluster is untouched, and the index no longer points to the tags.
        assert!(has(&out, &cluster(unknown)));
        assert!(!has(&out, &[0x53, 0xAB, 0x84, 0x12, 0x54, 0xC3, 0x67]));
        assert_eq!(identify(&out), Some(FormatId::Webm));
        assert_clean(&out);
    }
}

#[test]
fn a_checksum_over_changed_data_goes() {
    let data = webm(false, true, b"");
    let (out, _) = run(&data).unwrap();
    assert!(!has(&out, &el(CRC32, &[1, 2, 3, 4])));
    assert_clean(&out);
}

#[test]
fn data_after_the_segment_is_cut() {
    let data = webm(false, false, b"appended notes");
    let (out, findings) = run(&data).unwrap();
    assert_eq!(findings.last().unwrap().kind, FindingKind::TrailingData);
    assert_eq!(out.len(), data.len() - 14);
}

#[test]
fn chapters_keep_their_marks_and_lose_their_titles() {
    let display = [el(0x85, b"Arrival in Lisbon"), el(0x437C, b"eng")].concat();
    let atom = [el(0x73C4, &[1]), el(0x91, &[0]), el(0x80, &display)].concat();
    let chapters = el(CHAPTERS, &el(0x45B9, &el(0xB6, &atom)));
    let data = [
        header(),
        el(SEGMENT, &[info(false), chapters.clone()].concat()),
    ]
    .concat();
    let (out, findings) = run(&data).unwrap();
    let titles = findings
        .iter()
        .find(|f| f.location.to_string() == "Chapters > Titles");
    assert_eq!(
        titles.and_then(|f| f.value.clone()),
        Some(Value::Text("Arrival in Lisbon".into()))
    );
    assert!(!has(&out, b"Lisbon"));
    assert!(has(&out, b"eng"));
    assert_clean(&out);
}

#[test]
fn fonts_stay_and_other_attachments_go() {
    let file = |name: &[u8], media_type: &[u8]| {
        el(
            ATTACHED_FILE,
            &[
                el(0x466E, name),
                el(0x4660, media_type),
                el(0x465C, b"file data"),
                el(0x46AE, &[7]),
            ]
            .concat(),
        )
    };
    let attachments = el(
        ATTACHMENTS,
        &[
            file(b"Subtitles.ttf", b"application/x-truetype-font"),
            file(b"cover.jpg", b"image/jpeg"),
        ]
        .concat(),
    );
    let data = [header(), el(SEGMENT, &[info(false), attachments].concat())].concat();
    let (out, findings) = run(&data).unwrap();
    let attached: Vec<_> = findings
        .iter()
        .filter(|f| f.location.parts().first().map(String::as_str) == Some("Attachments"))
        .map(|f| (f.kind, f.disposition))
        .collect();
    assert_eq!(
        attached,
        [
            (
                FindingKind::EmbeddedFile,
                Disposition::Keep(KeepReason::RequiredToRender)
            ),
            (FindingKind::CoverArt, Disposition::Remove),
        ]
    );
    assert!(has(&out, b"Subtitles.ttf"));
    assert!(!has(&out, b"cover.jpg"));
    assert_clean(&out);
}

#[test]
fn other_documents_and_damage_are_refused() {
    let mut other = [el(EBML, &el(DOC_TYPE, b"other")), el(SEGMENT, &info(false))].concat();
    assert!(matches!(run(&other), Err(FormatError::Unsupported(_))));
    other[0] = 0;
    assert!(run(&other).is_err());
    let data = webm(false, false, b"");
    for len in [0, 3, 10, 30, data.len() - 1] {
        assert!(run(&data[..len]).is_err(), "cut at {len}");
    }
}
