//! Matroska and WebM videos made by `ffmpeg` (no threads, `bitexact`): a
//! clean WebM, a WebM written as a browser or OBS does (without sizes, to a
//! pipe), and an MKV with everything Matroska can carry about a person.

use std::fs;
use std::path::Path;
use std::process::Command;

use crate::media::ffmpeg;

const VIDEO: &[&str] = &["-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1"];
const SOUND: &[&str] = &["-f", "lavfi", "-i", "sine=frequency=440:duration=1"];
const WEBM: &[&str] = &[
    "-c:v",
    "libvpx",
    "-deadline",
    "realtime",
    "-b:v",
    "100k",
    "-c:a",
    "libopus",
    "-b:a",
    "32k",
    "-threads",
    "1",
    "-fflags",
    "+bitexact",
    "-flags",
    "+bitexact",
];

pub fn write(dir: &Path, cover: &Path) {
    let work = std::env::temp_dir().join("metascrub-matroska-fixtures");
    fs::create_dir_all(&work).unwrap();

    // Nothing about anyone. Encoding names the encoders in tags, so the
    // video is copied once more without them; the app names ffmpeg
    // requires are blanked afterwards.
    let encoded = work.join("encoded.webm");
    ffmpeg(&encoded, &[VIDEO, SOUND, WEBM]);
    let clean = dir.join("clean.webm");
    ffmpeg(
        &clean,
        &[
            &[
                "-i",
                encoded.to_str().unwrap(),
                "-c",
                "copy",
                "-map_metadata",
                "-1",
            ],
            &["-map_metadata:s:v", "-1", "-map_metadata:s:a", "-1"],
            &["-fflags", "+bitexact"],
        ],
    );
    let mut data = fs::read(&clean).unwrap();
    let head = data.len().min(512);
    let mut at = 0;
    while let Some(found) = data[at..head].windows(4).position(|w| w == b"Lavf") {
        data[at + found..at + found + 4].fill(0);
        at += found + 4;
    }
    fs::write(&clean, data).unwrap();

    // A browser or OBS recording: written to a pipe, so without sizes.
    let output = Command::new("ffmpeg")
        .args(["-v", "error"])
        .args(VIDEO)
        .args(SOUND)
        .args(WEBM)
        .args([
            "-metadata",
            "title=Meeting with Jane",
            "-f",
            "webm",
            "pipe:1",
        ])
        .output()
        .expect("ffmpeg is needed for the Matroska fixtures");
    assert!(output.status.success(), "ffmpeg (live WebM) failed");
    fs::write(dir.join("live.webm"), &output.stdout).unwrap();
    println!("wrote {}", dir.join("live.webm").display());

    let subtitles = work.join("subtitles.srt");
    fs::write(
        &subtitles,
        "1\n00:00:00,000 --> 00:00:00,900\nHappy birthday, Jane!\n",
    )
    .unwrap();
    let chapters = work.join("chapters.txt");
    fs::write(
        &chapters,
        ";FFMETADATA1\ntitle=Jane's birthday\nartist=Jane Doe\ncomment=Filmed at home in Lisbon\n\
         date=2026-03-14\n\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=Arrival in Lisbon\n\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Cake with Grandma Rosa\n",
    )
    .unwrap();
    // Not a real font: cleaning never reads inside one.
    let font = work.join("JaneHand.ttf");
    fs::write(&font, b"\0\x01\0\0 a font for styled subtitles").unwrap();
    let notes = work.join("notes.txt");
    fs::write(&notes, "Jane's shooting notes: camera serial 4012345").unwrap();
    ffmpeg(
        &dir.join("movie.mkv"),
        &[
            VIDEO,
            SOUND,
            &["-i", subtitles.to_str().unwrap()],
            &["-i", chapters.to_str().unwrap()],
            &["-map", "0", "-map", "1", "-map", "2", "-map_metadata", "3"],
            &["-map_chapters", "3"],
            &[
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-b:a",
                "32k",
                "-c:s",
                "srt",
                "-threads",
                "1",
                "-fflags",
                "+bitexact",
                "-flags",
                "+bitexact",
            ],
            &["-metadata", "creation_time=2026-03-14T09:26:52Z"],
            &["-metadata:s:v:0", "title=Jane's phone camera"],
            &["-metadata:s:a:0", "title=Jane's microphone"],
            &[
                "-attach",
                font.to_str().unwrap(),
                "-metadata:s:t:0",
                "mimetype=font/ttf",
            ],
            &[
                "-attach",
                cover.to_str().unwrap(),
                "-metadata:s:t:1",
                "mimetype=image/jpeg",
            ],
            &["-metadata:s:t:1", "filename=cover.jpg"],
            &[
                "-attach",
                notes.to_str().unwrap(),
                "-metadata:s:t:2",
                "mimetype=text/plain",
            ],
        ],
    );
}
