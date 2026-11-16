// Turns the user's cover choice into one image file every output can attach.
use crate::chapters::AudioFile;
use crate::meta::Cover;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

// Containers FFmpeg can attach a picture to with stream copy.
pub fn supported(ext: &str) -> bool {
    ["mp3", "m4a", "m4b"].contains(&ext.to_lowercase().as_str())
}

// "jpg" or "png", from the file's first bytes (its name may lie).
pub fn sniff(path: &Path) -> Result<&'static str, String> {
    let mut head = [0u8; 8];
    let n = File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .map_err(|e| format!("Can't read the cover image '{}': {}", path.display(), e))?;
    match &head[..n] {
        [0xFF, 0xD8, 0xFF, ..] => Ok("jpg"),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A] => Ok("png"),
        _ => Err(format!("'{}' is not a JPEG or PNG image.", path.display())),
    }
}

// Fails on a chosen image that is missing or not a JPEG/PNG, before anything is written.
pub fn validate(cover: &Cover) -> Result<(), String> {
    match cover {
        Cover::File(path) => sniff(path).map(|_| ()),
        _ => Ok(()),
    }
}

fn ext_for(codec: &str) -> Option<&'static str> {
    match codec {
        "mjpeg" => Some("jpg"),
        "png" => Some("png"),
        _ => None,
    }
}

// The first file with a picture stream, and that stream's codec. Any picture stream counts,
// not only `attached_pic`: Audible-style M4Bs keep the cover as a plain MJPEG track.
pub fn find_embedded(files: &[AudioFile]) -> Option<(usize, String)> {
    files.iter().enumerate().find_map(|(i, f)| Some((i, picture_codec(&f.path)?)))
}

fn picture_codec(path: &Path) -> Option<String> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name", "-of", "csv=p=0"])
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|codec| !codec.is_empty())
}

// Copies the first picture stream of `source` to `dest` without re-encoding.
fn extract(source: &Path, dest: &Path) -> Result<(), String> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .args(["-map", "0:v:0", "-frames:v", "1", "-c", "copy", "-y"])
        .arg(dest)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run ffmpeg: {}", e))?;
    if out.status.success() {
        return Ok(());
    }
    let _ = std::fs::remove_file(dest);
    Err(String::from_utf8_lossy(&out.stderr).into_owned())
}

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn scratch_path(ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!("abs-cover-{}-{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed), ext))
}

// The image to attach to every output (if any). Dropping it deletes an extracted copy.
pub struct Resolved {
    path: Option<PathBuf>,
    temp: bool,               // The file is ours to delete.
    pub note: Option<String>, // Why `Keep` ended up with no artwork, when the user should hear it.
}

impl Resolved {
    fn none(note: Option<String>) -> Resolved {
        Resolved { path: None, temp: false, note }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl Drop for Resolved {
    fn drop(&mut self) {
        if let Some(path) = self.path.as_ref().filter(|_| self.temp) {
            let _ = std::fs::remove_file(path);
        }
    }
}

// Errors only for a chosen image that is unusable; `Keep` problems come back as a note.
pub fn resolve(cover: &Cover, files: &[AudioFile]) -> Result<Resolved, String> {
    match cover {
        Cover::None => Ok(Resolved::none(None)),
        Cover::File(path) => {
            sniff(path)?;
            Ok(Resolved { path: Some(path.clone()), temp: false, note: None })
        }
        Cover::Keep => {
            let Some((i, codec)) = find_embedded(files) else { return Ok(Resolved::none(None)) };
            let name = crate::pipeline::file_name(&files[i].path);
            let Some(ext) = ext_for(&codec) else {
                return Ok(Resolved::none(Some(format!("The cover in {} is {}, not JPEG or PNG; writing no artwork.", name, codec))));
            };
            let dest = scratch_path(ext);
            match extract(&files[i].path, &dest) {
                Ok(()) => Ok(Resolved { path: Some(dest), temp: true, note: None }),
                Err(e) => Ok(Resolved::none(Some(format!("Couldn't read the cover from {}; writing no artwork.\n{}", name, e)))),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{audio_with_cover, cover_color, image, temp_dir};

    fn audio(path: std::path::PathBuf) -> AudioFile {
        AudioFile { path, silences: vec![], duration: 6.0 }
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn sniff_reads_the_bytes_not_the_name() {
        let dir = temp_dir("cover-sniff");
        write(&dir.join("a.png"), &[0xFF, 0xD8, 0xFF, 0xE0, 0, 0]); // JPEG bytes named .png
        write(&dir.join("b.jpg"), &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]);
        write(&dir.join("c.png"), b"GIF89a....");
        write(&dir.join("d.png"), b"");
        assert_eq!(sniff(&dir.join("a.png")), Ok("jpg"));
        assert_eq!(sniff(&dir.join("b.jpg")), Ok("png"));
        assert!(sniff(&dir.join("c.png")).unwrap_err().contains("not a JPEG or PNG"));
        assert!(sniff(&dir.join("d.png")).is_err());
        assert!(sniff(&dir.join("missing.png")).unwrap_err().contains("missing.png"));
    }

    #[test]
    fn validate_only_checks_a_chosen_file() {
        assert!(validate(&Cover::Keep).is_ok());
        assert!(validate(&Cover::None).is_ok());
        assert!(validate(&Cover::File("nope.png".into())).is_err());
    }

    #[test]
    fn only_mp3_and_m4_containers_take_artwork() {
        assert!(supported("mp3") && supported("M4B") && supported("m4a"));
        assert!(!supported("wav") && !supported("flac") && !supported("ogg"));
    }

    #[test]
    fn jpeg_and_png_codecs_map_to_extensions() {
        assert_eq!(ext_for("mjpeg"), Some("jpg"));
        assert_eq!(ext_for("png"), Some("png"));
        assert_eq!(ext_for("bmp"), None);
    }

    #[test]
    fn finds_the_first_file_with_a_cover() {
        let dir = temp_dir("cover-find");
        let red = dir.join("red.jpg");
        image(&red, "red");
        let (plain, covered) = (dir.join("a.mp3"), dir.join("b.m4b"));
        audio_with_cover(&plain, 6, None);
        audio_with_cover(&covered, 6, Some(&red));
        assert_eq!(find_embedded(&[audio(plain.clone())]), None);
        assert_eq!(find_embedded(&[audio(plain), audio(covered)]), Some((1, "mjpeg".to_string())));
    }

    #[test]
    fn keep_extracts_the_cover_and_cleans_up() {
        let dir = temp_dir("cover-keep");
        let red = dir.join("red.jpg");
        image(&red, "red");
        let book = dir.join("book.m4b");
        audio_with_cover(&book, 6, Some(&red));
        let resolved = resolve(&Cover::Keep, &[audio(book)]).unwrap();
        let path = resolved.path().unwrap().to_path_buf();
        assert_eq!(cover_color(&path), Some("red"));
        assert!(resolved.note.is_none());
        drop(resolved);
        assert!(!path.exists(), "the extracted cover is removed on drop");
    }

    #[test]
    fn keep_with_no_embedded_cover_is_quietly_nothing() {
        let dir = temp_dir("cover-keep-none");
        let book = dir.join("book.mp3");
        audio_with_cover(&book, 6, None);
        let resolved = resolve(&Cover::Keep, &[audio(book)]).unwrap();
        assert!(resolved.path().is_none() && resolved.note.is_none());
    }

    #[test]
    fn none_and_file_resolve_as_chosen() {
        let dir = temp_dir("cover-choose");
        let blue = dir.join("blue.png");
        image(&blue, "blue");
        assert!(resolve(&Cover::None, &[]).unwrap().path().is_none());
        let resolved = resolve(&Cover::File(blue.clone()), &[]).unwrap();
        assert_eq!(resolved.path(), Some(blue.as_path()));
        drop(resolved);
        assert!(blue.exists(), "a chosen image is never deleted");
        assert!(resolve(&Cover::File(dir.join("nope.png")), &[]).is_err());
    }
}
