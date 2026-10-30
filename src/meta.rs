// Per-book tags and track naming: every output file is one track of disc 1.
use crate::chapters::{sanitize_file_name, Segment};
use serde::{Deserialize, Serialize};

pub const DEFAULT_TEMPLATE: &str = "{n} - {title}";

// Empty fields keep whatever tag the source file had.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metadata {
    pub album: String,
    pub artist: String,
    pub album_artist: String,
    pub genre: String,
    pub year: String,
    pub template: String, // File name and title, with {n}, {title} and {album}.
}

// One output file: its name (without extension) and the tags to write.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub name: String,
    pub tags: Vec<(String, String)>,
}

// Names and tags every chapter of the plan, in order. `{n}` counts from 00 (books
// nearly always open with an intro); the track tag counts from 1, as players expect.
pub fn tracks(plan: &[Segment], meta: &Metadata) -> Result<Vec<Track>, String> {
    if meta.template.trim().is_empty() {
        return Err("Enter a track naming convention, such as {n} - {title}.".into());
    }
    let total = plan.len();
    let width = total.saturating_sub(1).to_string().len().max(2);
    let shared = [
        ("album", &meta.album),
        ("artist", &meta.artist),
        ("album_artist", &meta.album_artist),
        ("genre", &meta.genre),
        ("date", &meta.year),
    ];
    Ok(plan
        .iter()
        .enumerate()
        .map(|(n, segment)| {
            // {title} last, so a title containing "{n}" stays as written.
            let name = meta
                .template
                .replace("{n}", &format!("{:0width$}", n))
                .replace("{album}", meta.album.trim())
                .replace("{title}", &segment.name)
                .trim()
                .to_string();
            let mut tags = vec![
                ("title".to_string(), name.clone()),
                ("track".to_string(), format!("{}/{}", n + 1, total)),
                ("disc".to_string(), "1/1".to_string()),
            ];
            tags.extend(shared.iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.to_string(), v.trim().to_string())));
            Track { name: sanitize_file_name(&name), tags }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(titles: &[&str]) -> Vec<Segment> {
        titles.iter().map(|t| Segment { name: t.to_string(), pieces: vec![] }).collect()
    }

    fn meta(template: &str) -> Metadata {
        Metadata { album: "Moby: Dick".into(), artist: "Melville".into(), template: template.into(), ..Default::default() }
    }

    #[test]
    fn tracks_number_from_zero_on_disc_one() {
        let tracks = tracks(&plan(&["Opening", "Chapter: One", "Two"]), &meta(DEFAULT_TEMPLATE)).unwrap();
        let names: Vec<&str> = tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["00 - Opening", "01 - Chapter - One", "02 - Two"]);

        let tag = |t: &Track, k: &str| t.tags.iter().find(|(key, _)| key == k).map(|(_, v)| v.clone());
        let second = &tracks[1];
        assert_eq!(tag(second, "title").as_deref(), Some("01 - Chapter: One"));
        assert_eq!(tag(second, "track").as_deref(), Some("2/3"));
        assert_eq!(tag(second, "disc").as_deref(), Some("1/1"));
        assert_eq!(tag(second, "album").as_deref(), Some("Moby: Dick"));
        assert_eq!(tag(second, "artist").as_deref(), Some("Melville"));
        assert_eq!(tag(second, "genre"), None, "empty fields keep the source tag");
    }

    #[test]
    fn template_tokens_and_padding() {
        let titles: Vec<String> = (0..120).map(|i| format!("T{}", i)).collect();
        let titles: Vec<&str> = titles.iter().map(String::as_str).collect();
        let tracks = tracks(&plan(&titles), &meta("{album} {n}. {title}")).unwrap();
        assert_eq!(tracks[0].name, "Moby - Dick 000. T0");
        assert_eq!(tracks[119].name, "Moby - Dick 119. T119");
    }

    #[test]
    fn empty_template_is_an_error() {
        assert!(tracks(&plan(&["a"]), &meta("  ")).is_err());
    }
}
