//! Music: MP3 files from `ffmpeg` (LAME), with the tags ffmpeg writes, and
//! the tags it can't write (APEv2, Lyrics3, ID3v2.4 with a footer) built
//! by hand around the same sound.

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
}
