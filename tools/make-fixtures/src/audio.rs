//! Music: MP3, FLAC, Ogg Vorbis and Opus files from `ffmpeg`, with the tags
//! ffmpeg writes, and the tags it can't write (APEv2, Lyrics3, ID3v2.4 with
//! a footer, ID3 around FLAC) built by hand around the same sound.

use std::fs;
use std::path::Path;

use crate::media::ffmpeg;

const SOUND: &[&str] = &["-f", "lavfi", "-i", "sine=frequency=440:duration=1"];
const MP3: &[&str] = &[
    "-c:a",
    "libmp3lame",
    "-b:a",
    "64k",
    "-ac",
    "1",
    "-threads",
    "1",
    "-fflags",
    "+bitexact",
    "-flags",
    "+bitexact",
];

const FLAC: &[&str] = &[
    "-c:a",
    "flac",
    "-threads",
    "1",
    "-fflags",
    "+bitexact",
    "-flags",
    "+bitexact",
];

const VORBIS: &[&str] = &[
    "-c:a",
    "libvorbis",
    "-threads",
    "1",
    "-fflags",
    "+bitexact",
    "-flags",
    "+bitexact",
];

const OPUS: &[&str] = &[
    "-c:a",
    "libopus",
    "-threads",
    "1",
    "-fflags",
    "+bitexact",
    "-flags",
    "+bitexact",
];

const TAGS: &[&str] = &[
    "-metadata",
    "title=Holiday",
    "-metadata",
    "artist=Jane Doe",
    "-metadata",
    "date=2026-03-14",
    "-metadata",
    "comment=Recorded at home",
];

/// Ogg's CRC-32: polynomial 0x04C11DB7, not reflected, no initial value.
fn ogg_crc(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |crc, &byte| {
        (0..8).fold(crc ^ (u32::from(byte) << 24), |c, _| {
            if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            }
        })
    })
}

/// Where each Ogg page starts and ends.
fn ogg_pages(data: &[u8]) -> Vec<(usize, usize)> {
    let mut pages = Vec::new();
    let mut pos = 0;
    while pos < data.len() {
        let segments = usize::from(data[pos + 26]);
        let body: usize = data[pos + 27..pos + 27 + segments]
            .iter()
            .map(|&l| usize::from(l))
            .sum();
        let end = pos + 27 + segments + body;
        pages.push((pos, end));
        pos = end;
    }
    pages
}

/// The page with `header` (its first 26 bytes) holding `packets`.
fn ogg_page(header: &[u8], packets: &[Vec<u8>]) -> Vec<u8> {
    let mut lacing = Vec::new();
    for packet in packets {
        lacing.extend(std::iter::repeat_n(255u8, packet.len() / 255));
        lacing.push((packet.len() % 255) as u8);
    }
    let mut page = header[..22].to_vec();
    page.extend_from_slice(&[0; 4]);
    page.push(u8::try_from(lacing.len()).unwrap());
    page.extend_from_slice(&lacing);
    page.extend_from_slice(&packets.concat());
    let crc = ogg_crc(&page);
    page[22..26].copy_from_slice(&crc.to_le_bytes());
    page
}

/// The packets that end on the page at `start..end` (ffmpeg's header
/// pages hold whole packets).
fn ogg_packets(data: &[u8], start: usize, end: usize) -> Vec<Vec<u8>> {
    let segments = usize::from(data[start + 26]);
    let mut at = start + 27 + segments;
    let mut packets = Vec::new();
    let mut packet = Vec::new();
    for &lace in &data[start + 27..start + 27 + segments] {
        packet.extend_from_slice(&data[at..at + usize::from(lace)]);
        at += usize::from(lace);
        if lace < 255 {
            packets.push(std::mem::take(&mut packet));
        }
    }
    assert_eq!(at, end);
    packets
}

/// `data` with an empty comment packet (no encoder name, no tags) in
/// place of the one ffmpeg wrote, which starts the second page.
fn ogg_without_tags(data: &[u8], magic: &[u8], framing: bool) -> Vec<u8> {
    let pages = ogg_pages(data);
    let (start, end) = pages[1];
    let mut packets = ogg_packets(data, start, end);
    assert!(packets[0].starts_with(magic));
    packets[0] = [magic, &[0; 8], if framing { &[1] } else { &[] }].concat();
    [
        &data[..start],
        &ogg_page(&data[start..start + 26], &packets),
        &data[end..],
    ]
    .concat()
}

/// `data` with Opus's output gain set to `gain` (in 1/256 dB).
fn opus_with_gain(data: &[u8], gain: i16) -> Vec<u8> {
    let (start, end) = ogg_pages(data)[0];
    let mut head = ogg_packets(data, start, end).remove(0);
    head[16..18].copy_from_slice(&gain.to_le_bytes());
    [
        ogg_page(&data[start..start + 26], &[head]).as_slice(),
        &data[end..],
    ]
    .concat()
}

/// `data` without its FLAC metadata blocks, except the stream description:
/// ffmpeg always adds a comment block and padding.
fn bare_flac(data: &[u8]) -> Vec<u8> {
    let mut pos = 4;
    loop {
        let last = data[pos] & 0x80 != 0;
        let len = u32::from_be_bytes([0, data[pos + 1], data[pos + 2], data[pos + 3]]) as usize;
        pos += 4 + len;
        if last {
            break;
        }
    }
    let mut streaminfo = data[4..42].to_vec();
    streaminfo[0] |= 0x80;
    [b"fLaC".as_slice(), &streaminfo, &data[pos..]].concat()
}

fn write(path: &Path, data: &[u8]) {
    fs::write(path, data).unwrap();
    println!("wrote {}", path.display());
}

fn syncsafe(size: usize) -> [u8; 4] {
    let size = u32::try_from(size).unwrap();
    [
        (size >> 21) as u8 & 0x7F,
        (size >> 14) as u8 & 0x7F,
        (size >> 7) as u8 & 0x7F,
        size as u8 & 0x7F,
    ]
}

/// An ID3v2.4 tag with a footer, as some taggers append after the sound.
fn id3v24_with_footer(frames: &[(&[u8; 4], &str)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, text) in frames {
        let data = [&[3u8][..], text.as_bytes()].concat();
        body.extend_from_slice(*id);
        body.extend_from_slice(&syncsafe(data.len()));
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(&data);
    }
    let header = [b"ID3".as_slice(), &[4, 0, 0x10], &syncsafe(body.len())].concat();
    let footer = [b"3DI".as_slice(), &[4, 0, 0x10], &syncsafe(body.len())].concat();
    [header, body, footer].concat()
}

fn ape(items: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (key, value, flags) in items {
        body.extend_from_slice(&u32::try_from(value.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&flags.to_le_bytes());
        body.extend_from_slice(key.as_bytes());
        body.push(0);
        body.extend_from_slice(value);
    }
    let size = u32::try_from(body.len() + 32).unwrap();
    let count = u32::try_from(items.len()).unwrap();
    let footer = [
        b"APETAGEX".as_slice(),
        &2000u32.to_le_bytes(),
        &size.to_le_bytes(),
        &count.to_le_bytes(),
        &0u32.to_le_bytes(),
        &[0; 8],
    ]
    .concat();
    [body, footer].concat()
}

fn lyrics3(lyrics: &str) -> Vec<u8> {
    let field = format!("LYR{:05}{lyrics}", lyrics.len());
    let body = format!("LYRICSBEGIN{field}");
    format!("{body}{:06}LYRICS200", body.len()).into_bytes()
}

pub fn write_all(dir: &Path, jpeg_dir: &Path) {
    // No tags: only the Xing frame LAME writes for the length.
    ffmpeg(
        &dir.join("clean.mp3"),
        &[
            SOUND,
            MP3,
            &[
                "-map_metadata",
                "-1",
                "-id3v2_version",
                "0",
                "-write_id3v1",
                "0",
            ],
        ],
    );
    // What a music app writes: ID3v2.3 with a cover, ID3v1 at the end.
    let cover = jpeg_dir.join("clean.jpg");
    ffmpeg(
        &dir.join("tagged.mp3"),
        &[
            SOUND,
            &[
                "-i",
                cover.to_str().unwrap(),
                "-map",
                "0",
                "-map",
                "1",
                "-c:v",
                "copy",
            ],
            MP3,
            &[
                "-id3v2_version",
                "3",
                "-write_id3v1",
                "1",
                "-disposition:v",
                "attached_pic",
                "-metadata",
                "title=Holiday",
                "-metadata",
                "artist=Jane Doe",
                "-metadata",
                "album=Summer recordings",
                "-metadata",
                "date=2026-03-14",
                "-metadata",
                "comment=Recorded at home",
                "-metadata",
                "REPLAYGAIN_TRACK_GAIN=-6.20 dB",
            ],
        ],
    );

    let clean = fs::read(dir.join("clean.mp3")).unwrap();
    // APEv2 and Lyrics3, as foobar2000 and older taggers add them, before
    // an ID3v1 block.
    let mut id3v1 = vec![0u8; 128];
    id3v1[..3].copy_from_slice(b"TAG");
    id3v1[3..10].copy_from_slice(b"Holiday");
    id3v1[33..41].copy_from_slice(b"Jane Doe");
    id3v1[127] = 0xFF;
    let appended = [
        clean.as_slice(),
        &ape(&[
            ("Artist", b"Jane Doe", 0),
            ("Comment", b"Call with the lawyer", 0),
            ("Cover Art (Front)", b"cover.jpg\0\xFF\xD8\xFF\xD9", 2),
        ]),
        &lyrics3("Walking down the street"),
        &id3v1,
    ]
    .concat();
    write(&dir.join("appended.mp3"), &appended);
    write(
        &dir.join("footer.mp3"),
        &[
            clean.as_slice(),
            &id3v24_with_footer(&[(b"TPE1", "Jane Doe"), (b"TIT2", "Holiday")]),
        ]
        .concat(),
    );
    // Data hidden between two frames: refused, never guessed at.
    let half = clean.len() / 2;
    let frame_start = (half..clean.len())
        .find(|&i| clean[i] == 0xFF && clean[i + 1] & 0xE0 == 0xE0)
        .unwrap();
    let spliced = [
        &clean[..frame_start],
        b"hidden between frames",
        &clean[frame_start..],
    ]
    .concat();
    write(&dir.join("spliced.mp3"), &spliced);

    // FLAC: what ffmpeg writes with no tags (its name and padding), with
    // tags and a cover, and with ID3 tags around it as some taggers add.
    ffmpeg(
        &dir.join("plain.flac"),
        &[SOUND, FLAC, &["-map_metadata", "-1"]],
    );
    ffmpeg(
        &dir.join("tagged.flac"),
        &[
            SOUND,
            &[
                "-i",
                cover.to_str().unwrap(),
                "-map",
                "0",
                "-map",
                "1",
                "-c:v",
                "copy",
            ],
            FLAC,
            &[
                "-disposition:v",
                "attached_pic",
                "-metadata",
                "title=Holiday",
                "-metadata",
                "artist=Jane Doe",
                "-metadata",
                "date=2026-03-14",
                "-metadata",
                "comment=Recorded at home",
                "-metadata",
                "REPLAYGAIN_TRACK_GAIN=-6.20 dB",
            ],
        ],
    );
    let bare = bare_flac(&fs::read(dir.join("plain.flac")).unwrap());
    write(&dir.join("clean.flac"), &bare);
    let id3 = [
        b"ID3".as_slice(),
        &[3, 0, 0],
        &syncsafe(15),
        b"TPE1\0\0\0\x05\0\0\0Jane",
    ]
    .concat();
    write(
        &dir.join("id3.flac"),
        &[id3.as_slice(), &bare, &id3v1].concat(),
    );
    let half = bare.len() / 2;
    let frame_start = (half..bare.len())
        .find(|&i| bare[i] == 0xFF && bare[i + 1] & 0xFE == 0xF8)
        .unwrap();
    let spliced = [
        &bare[..frame_start],
        b"hidden between frames",
        &bare[frame_start..],
    ]
    .concat();
    write(&dir.join("spliced.flac"), &spliced);

    // Ogg: Vorbis and Opus with tags (and a picture, as taggers store it),
    // the same without, an Opus file with a volume adjustment, and files
    // refused: data hidden between pages, two streams one after another.
    let picture = format!(
        "METADATA_BLOCK_PICTURE={}",
        "AAAAAwAAAAppbWFnZS9qcGVnAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAE/9j/2Q=="
    );
    ffmpeg(
        &dir.join("tagged.ogg"),
        &[
            SOUND,
            VORBIS,
            TAGS,
            &[
                "-metadata",
                "REPLAYGAIN_TRACK_GAIN=-6.20 dB",
                "-metadata",
                &picture,
            ],
        ],
    );
    ffmpeg(&dir.join("tagged.opus"), &[SOUND, OPUS, TAGS]);
    let vorbis = fs::read(dir.join("tagged.ogg")).unwrap();
    let clean_vorbis = ogg_without_tags(&vorbis, b"\x03vorbis", true);
    write(&dir.join("clean.ogg"), &clean_vorbis);
    let opus = fs::read(dir.join("tagged.opus")).unwrap();
    let clean_opus = ogg_without_tags(&opus, b"OpusTags", false);
    write(&dir.join("clean.opus"), &clean_opus);
    // -3 dB.
    write(&dir.join("gain.opus"), &opus_with_gain(&clean_opus, -768));
    let (_, header_end) = ogg_pages(&clean_vorbis)[1];
    let spliced = [
        &clean_vorbis[..header_end],
        b"hidden between pages",
        &clean_vorbis[header_end..],
    ]
    .concat();
    write(&dir.join("spliced.ogg"), &spliced);
    write(
        &dir.join("chained.opus"),
        &[clean_opus.as_slice(), &clean_opus].concat(),
    );
}
