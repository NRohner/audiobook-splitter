use super::super::time::clock;
use audiobook_splitter_rust::cover::validate;
use audiobook_splitter_rust::meta::{Cover, Metadata};
use audiobook_splitter_rust::plan::{Plan, PlanBreak};
use std::cmp::Ordering;
#[cfg(test)]
use audiobook_splitter_rust::chapters::AudioFile;
#[cfg(test)]
use audiobook_splitter_rust::plan::{Findings, PlanSilence, Settings};

pub(crate) type Pos = (usize, f64); // (file index, seconds into that file)

pub(crate) fn pos(b: &PlanBreak) -> Pos {
    (b.file, b.at)
}

pub(crate) fn order(a: Pos, b: Pos) -> Ordering {
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
}

pub(crate) fn same(a: Pos, b: Pos) -> bool {
    a.0 == b.0 && (a.1 - b.1).abs() < 0.001
}

// The first chapter starts the book; it always exists.
pub(crate) fn is_fixed(b: &PlanBreak) -> bool {
    b.file == 0 && b.at <= 0.01
}

// Whether the chapters or tags differ from what the editor started with.
pub(crate) fn changed(plan: &Plan, original: &(Vec<PlanBreak>, Metadata)) -> bool {
    plan.findings.breaks != original.0 || plan.metadata != original.1
}

pub(crate) fn add(plan: &mut Plan, file: usize, at: f64, title: &str) -> Result<(), String> {
    let duration = plan.files.get(file).ok_or("There is no such audio file.")?.duration;
    if !(at >= 0.0 && at < duration) {
        return Err(format!("Enter a time between 0:00:00 and {}.", clock(duration)));
    }
    let title = title.trim();
    if title.is_empty() {
        return Err("A chapter needs a title.".into());
    }
    if file == 0 && at <= 0.01 {
        return Err("The book already starts with a chapter at 0:00:00.".into());
    }
    let breaks = &mut plan.findings.breaks;
    if breaks.iter().any(|b| same(pos(b), (file, at))) {
        return Err("There is already a chapter there.".into());
    }
    breaks.push(PlanBreak { file, at, title: title.into(), score: None });
    breaks.sort_by(|a, b| order(pos(a), pos(b)));
    Ok(())
}

pub(crate) fn rename(plan: &mut Plan, i: usize, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("A chapter needs a title.".into());
    }
    plan.findings.breaks.get_mut(i).ok_or("No such chapter.")?.title = title.into();
    Ok(())
}

pub(crate) fn remove(plan: &mut Plan, i: usize) -> Result<(), String> {
    if is_fixed(plan.findings.breaks.get(i).ok_or("No such chapter.")?) {
        return Err("The first chapter starts the book and can't be removed.".into());
    }
    plan.findings.breaks.remove(i);
    Ok(())
}

// The silence break `i` would move to: the previous (`dir` < 0) or next one, staying
// between its neighbouring breaks.
pub(crate) fn neighbour(plan: &Plan, i: usize, dir: i32) -> Option<Pos> {
    let breaks = &plan.findings.breaks;
    let here = pos(breaks.get(i)?);
    let prev = i.checked_sub(1).map(|p| pos(&breaks[p]));
    let next = breaks.get(i + 1).map(pos);
    let ok = |s: Pos| prev.is_none_or(|p| order(s, p).is_gt()) && next.is_none_or(|n| order(s, n).is_lt());
    let silences = plan.findings.silences.iter().map(|s| (s.file, s.at));
    if dir < 0 {
        silences.filter(|&s| order(s, here).is_lt() && ok(s)).last()
    } else {
        silences.into_iter().find(|&s| order(s, here).is_gt() && ok(s))
    }
}

pub(crate) fn move_break(plan: &mut Plan, i: usize, dir: i32) -> Result<(), String> {
    if is_fixed(plan.findings.breaks.get(i).ok_or("No such chapter.")?) {
        return Err("The first chapter starts the book and can't be moved.".into());
    }
    let (file, at) = neighbour(plan, i, dir).ok_or("There is no silence to move to.")?;
    let b = &mut plan.findings.breaks[i];
    (b.file, b.at, b.score) = (file, at, None);
    Ok(())
}

// Silences the analysis skipped: those where no chapter starts.
pub(crate) fn unused_silences(plan: &Plan) -> Vec<Pos> {
    plan.findings
        .silences
        .iter()
        .map(|s| (s.file, s.at))
        .filter(|&s| !plan.findings.breaks.iter().any(|b| same(pos(b), s)))
        .collect()
}

// Sets the cover choice, refusing an image that isn't a readable JPEG or PNG.
pub(crate) fn set_cover(plan: &mut Plan, cover: Cover) -> Result<(), String> {
    validate(&cover)?;
    plan.metadata.cover = cover;
    Ok(())
}

// What Whisper heard either side of a break, if it transcribed there.
pub(crate) fn heard(plan: &Plan, i: usize) -> Option<(&str, &str)> {
    let at = pos(plan.findings.breaks.get(i)?);
    let s = plan.findings.silences.iter().find(|s| same((s.file, s.at), at))?;
    Some((s.before.as_str(), s.after.as_str())).filter(|(b, a)| !(b.is_empty() && a.is_empty()))
}

// One 100 s file, chapters at 0, 30 and 60 s ("One", "Two", "Three"), silences at 0, 10, 20,
// 30, 45, 60 and 80 s. Shared with the full-screen editor's tests.
#[cfg(test)]
pub(crate) fn sample_plan() -> Plan {
    let silence = |at: f64| PlanSilence { file: 0, at, before: String::new(), after: String::new() };
    let brk = |at: f64, title: &str| PlanBreak { file: 0, at, title: title.into(), score: None };
    Plan::new(
        vec![AudioFile { path: "a.mp3".into(), silences: vec![], duration: 100.0 }],
        Settings::default(),
        None,
        Findings {
            breaks: vec![brk(0.0, "One"), brk(30.0, "Two"), brk(60.0, "Three")],
            silences: [0.0, 10.0, 20.0, 30.0, 45.0, 60.0, 80.0].map(silence).to_vec(),
            ..Default::default()
        },
        Metadata::default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        sample_plan()
    }

    fn titles(p: &Plan) -> Vec<&str> {
        p.findings.breaks.iter().map(|b| b.title.as_str()).collect()
    }

    #[test]
    fn add_inserts_in_order_and_rejects_bad_positions() {
        let mut p = plan();
        add(&mut p, 0, 45.0, "New").unwrap();
        assert_eq!(titles(&p), ["One", "Two", "New", "Three"]);
        assert!(add(&mut p, 0, 45.0, "Again").unwrap_err().contains("already a chapter"));
        assert!(add(&mut p, 0, 100.0, "Late").is_err());
        assert!(add(&mut p, 0, -1.0, "Early").is_err());
        assert!(add(&mut p, 3, 5.0, "Nofile").is_err());
    }

    #[test]
    fn add_rejects_blank_titles_and_the_first_chapters_position() {
        let mut p = plan();
        assert!(add(&mut p, 0, 45.0, "  ").unwrap_err().contains("needs a title"));
        assert!(add(&mut p, 0, 0.005, "Dup").unwrap_err().contains("already starts"));
        assert_eq!(titles(&p), ["One", "Two", "Three"]);
        add(&mut p, 0, 45.0, "  New ").unwrap();
        assert_eq!(titles(&p)[2], "New");
    }

    #[test]
    fn changed_compares_breaks_and_metadata_with_the_original() {
        let mut p = plan();
        let original = (p.findings.breaks.clone(), p.metadata.clone());
        assert!(!changed(&p, &original));
        rename(&mut p, 1, "Storm").unwrap();
        assert!(changed(&p, &original));
        p.findings.breaks = original.0.clone();
        p.metadata.album = "Other".into();
        assert!(changed(&p, &original));
    }

    #[test]
    fn rename_trims_and_rejects_empty() {
        let mut p = plan();
        rename(&mut p, 1, "  Storm ").unwrap();
        assert_eq!(titles(&p)[1], "Storm");
        assert!(rename(&mut p, 1, "  ").is_err());
        assert!(rename(&mut p, 9, "x").is_err());
    }

    #[test]
    fn the_first_chapter_cannot_be_removed_or_moved() {
        let mut p = plan();
        assert!(remove(&mut p, 0).is_err());
        assert!(move_break(&mut p, 0, 1).is_err());
        remove(&mut p, 1).unwrap();
        assert_eq!(titles(&p), ["One", "Three"]);
        assert!(remove(&mut p, 5).is_err());
    }

    #[test]
    fn moves_go_to_the_adjacent_silence_but_not_past_a_neighbour() {
        let mut p = plan();
        // "Two" is at 30; the silence before it is 20, after it 45.
        assert_eq!(neighbour(&p, 1, -1), Some((0, 20.0)));
        assert_eq!(neighbour(&p, 1, 1), Some((0, 45.0)));
        move_break(&mut p, 1, 1).unwrap();
        assert_eq!(p.findings.breaks[1].at, 45.0);
        // Next silence after 45 is 60, which is "Three": not allowed to land on it.
        assert_eq!(neighbour(&p, 1, 1), None);
        assert!(move_break(&mut p, 1, 1).unwrap_err().contains("no silence"));
        move_break(&mut p, 1, -1).unwrap();
        assert_eq!(p.findings.breaks[1].at, 30.0);
    }

    #[test]
    fn moving_clears_the_match_score() {
        let mut p = plan();
        p.findings.breaks[1].score = Some(1.2);
        move_break(&mut p, 1, -1).unwrap();
        assert_eq!(p.findings.breaks[1].score, None);
    }

    #[test]
    fn unused_silences_skip_those_that_start_a_chapter() {
        let p = plan();
        let at: Vec<f64> = unused_silences(&p).into_iter().map(|(_, at)| at).collect();
        assert_eq!(at, [10.0, 20.0, 45.0, 80.0]);
    }

    #[test]
    fn set_cover_accepts_images_and_refuses_anything_else() {
        let dir = std::env::temp_dir().join(format!("abs-edit-cover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (png, text) = (dir.join("c.png"), dir.join("c.txt"));
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]).unwrap();
        std::fs::write(&text, "hi").unwrap();
        let mut p = plan();
        set_cover(&mut p, Cover::File(png.clone())).unwrap();
        assert_eq!(p.metadata.cover, Cover::File(png));
        assert!(set_cover(&mut p, Cover::File(text)).is_err());
        assert!(matches!(p.metadata.cover, Cover::File(_)), "a refused image leaves the old choice");
        set_cover(&mut p, Cover::None).unwrap();
        assert_eq!(p.metadata.cover, Cover::None);
    }
}
