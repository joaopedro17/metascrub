//! Videos, music and photos in the box format of MP4, QuickTime, HEIC and
//! AVIF. Their pictures and sound need real encoders, so these are made by
//! `ffmpeg` and `heif-enc` (with the x265 and aom plugins), run without
//! threads and with `bitexact` so the same versions give the same bytes.

use std::fs;
use std::path::Path;
use std::process::Command;

const VIDEO: &[&str] = &["-f", "lavfi", "-i", "testsrc=size=64x48:rate=10:duration=1"];
const SOUND: &[&str] = &["-f", "lavfi", "-i", "sine=frequency=440:duration=1"];
const ENCODE: &[&str] = &[
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
    "-threads",
    "1",
    "-fflags",
    "+bitexact",
    "-flags",
    "+bitexact",
];
const DATE: &str = "2026-03-14T09:26:52Z";

fn run(program: &str, args: &[&str]) {
    let status = Command::new(program)
        .args(args)
        .status()
        .expect("ffmpeg and heif-enc are needed for the media fixtures");
    assert!(status.success(), "{program} {args:?} failed");
}

fn ffmpeg(output: &Path, args: &[&[&str]]) {
    let mut all = vec!["-y", "-v", "error"];
    for group in args {
        all.extend_from_slice(group);
    }
    all.push(output.to_str().unwrap());
    run("ffmpeg", &all);
    println!("wrote {}", output.display());
}

fn heif(output: &Path, input: &Path, extra: &[&str]) {
    let mut args = vec!["-q", "50"];
    if extra.contains(&"-A") {
        args.extend(["-p", "threads=1"]);
    }
    args.extend_from_slice(extra);
    args.extend([input.to_str().unwrap(), "-o", output.to_str().unwrap()]);
    let output_text = Command::new("heif-enc")
        .args(&args)
        .output()
        .expect("heif-enc is needed for the media fixtures");
    // heif-enc reports a missing encoder but still exits successfully.
    assert!(
        output_text.status.success() && output.exists(),
        "heif-enc {args:?} failed: {}",
        String::from_utf8_lossy(&output_text.stderr)
    );
    println!("wrote {}", output.display());
}

pub fn write(dir: &Path, jpeg_dir: &Path, png_dir: &Path) {
    // No metadata at all: a blank encoder name, no tags, no dates.
    ffmpeg(
        &dir.join("clean.mp4"),
        &[
            VIDEO,
            ENCODE,
            &["-map_metadata", "-1", "-metadata:s:v:0", "encoder= "],
        ],
    );
    // What a phone's camera app writes, through ffmpeg's own tags.
    ffmpeg(
        &dir.join("phone.mp4"),
        &[
            VIDEO,
            SOUND,
            ENCODE,
            &[
                "-metadata",
                "location=+48.8584+002.2945/",
                "-metadata",
                "title=Holiday",
                "-metadata",
                "artist=Jane Doe",
                "-metadata",
                &format!("creation_time={DATE}"),
            ],
        ],
    );
    // An iPhone's keyed metadata, and a timecode track with the time of day.
    ffmpeg(
        &dir.join("iphone.mov"),
        &[
            VIDEO,
            ENCODE,
            &[
                "-movflags",
                "use_metadata_tags",
                "-metadata",
                "com.apple.quicktime.location.ISO6709=+48.8584+002.2945+035.000/",
                "-metadata",
                "com.apple.quicktime.make=Fictional Camera Co.",
                "-metadata",
                "com.apple.quicktime.model=FC-100",
                "-metadata",
                "com.apple.quicktime.software=PhotoTool 2.1",
                "-metadata",
                "com.apple.quicktime.creationdate=2026-03-14T10:26:52+0100",
                "-metadata",
                "com.apple.quicktime.content.identifier=5F1C2E9A-0B7D-4C3E-9A61-2D8F4B7E1C03",
                "-metadata",
                &format!("creation_time={DATE}"),
                "-timecode",
                "09:26:52:00",
            ],
        ],
    );
    // Music tags and cover art.
    let cover = png_dir.join("clean.png");
    ffmpeg(
        &dir.join("voice.m4a"),
        &[
            SOUND,
            &["-i", cover.to_str().unwrap()],
            ENCODE,
            &[
                "-map",
                "0",
                "-map",
                "1",
                "-c:v",
                "copy",
                "-disposition:v",
                "attached_pic",
                "-metadata",
                "title=Voice memo",
                "-metadata",
                "artist=Jane Doe",
                "-metadata",
                "comment=Call with the lawyer",
            ],
        ],
    );

    let clean = fs::read(dir.join("clean.mp4")).unwrap();
    let trailing = [clean.as_slice(), b"appended after the video"].concat();
    fs::write(dir.join("trailing.mp4"), trailing).unwrap();
    println!("wrote {}", dir.join("trailing.mp4").display());

    // Photos keep the JPEG's Exif and XMP, and get a small preview.
    let camera = jpeg_dir.join("camera.jpg");
    heif(&dir.join("camera.avif"), &camera, &["-A", "-t", "16"]);
    heif(
        &dir.join("clean.avif"),
        &jpeg_dir.join("clean.jpg"),
        &["-A"],
    );
    heif(
        &dir.join("xmp.avif"),
        &jpeg_dir.join("xmp-iptc.jpg"),
        &["-A"],
    );
    heif(&dir.join("camera.heic"), &camera, &["-t", "16"]);
}
