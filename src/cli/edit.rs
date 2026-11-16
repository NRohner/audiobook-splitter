// `edit`: change a plan's chapters and tags by hand. The functions up top are plain edits
// on the plan (unit-tested); the menu below only chooses which one to call.
use super::time::{clock, parse_time};
use super::{open_plan, place, prompt};
use audiobook_splitter_rust::cover::{find_embedded, validate};
use audiobook_splitter_rust::meta::{Cover, Metadata};
use audiobook_splitter_rust::pipeline::file_name;
use audiobook_splitter_rust::plan::{Plan, PlanBreak};
use audiobook_splitter_rust::user_path;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};
use std::cmp::Ordering;
use std::path::PathBuf;

type Pos = (usize, f64); // (file index, seconds into that file)

fn pos(b: &PlanBreak) -> Pos {
    (b.file, b.at)
}

fn order(a: Pos, b: Pos) -> Ordering {
    a.0.cmp(&b.0).then(a.1.total_cmp(&b.1))
}

fn same(a: Pos, b: Pos) -> bool {
    a.0 == b.0 && (a.1 - b.1).abs() < 0.001
}

// The first chapter starts the book; it always exists.
pub fn is_fixed(b: &PlanBreak) -> bool {
    b.file == 0 && b.at <= 0.01
}

// Whether the chapters or tags differ from what the editor started with.
fn changed(plan: &Plan, original: &(Vec<PlanBreak>, Metadata)) -> bool {
    plan.findings.breaks != original.0 || plan.metadata != original.1
}

pub fn add(plan: &mut Plan, file: usize, at: f64, title: &str) -> Result<(), String> {
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

pub fn rename(plan: &mut Plan, i: usize, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("A chapter needs a title.".into());
    }
    plan.findings.breaks.get_mut(i).ok_or("No such chapter.")?.title = title.into();
    Ok(())
}

pub fn remove(plan: &mut Plan, i: usize) -> Result<(), String> {
    if is_fixed(plan.findings.breaks.get(i).ok_or("No such chapter.")?) {
        return Err("The first chapter starts the book and can't be removed.".into());
    }
    plan.findings.breaks.remove(i);
    Ok(())
}

// The silence break `i` would move to: the previous (`dir` < 0) or next one, staying
// between its neighbouring breaks.
pub fn neighbour(plan: &Plan, i: usize, dir: i32) -> Option<Pos> {
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

pub fn move_break(plan: &mut Plan, i: usize, dir: i32) -> Result<(), String> {
    if is_fixed(plan.findings.breaks.get(i).ok_or("No such chapter.")?) {
        return Err("The first chapter starts the book and can't be moved.".into());
    }
    let (file, at) = neighbour(plan, i, dir).ok_or("There is no silence to move to.")?;
    let b = &mut plan.findings.breaks[i];
    (b.file, b.at, b.score) = (file, at, None);
    Ok(())
}

// Silences the analysis skipped: those where no chapter starts.
pub fn unused_silences(plan: &Plan) -> Vec<Pos> {
    plan.findings
        .silences
        .iter()
        .map(|s| (s.file, s.at))
        .filter(|&s| !plan.findings.breaks.iter().any(|b| same(pos(b), s)))
        .collect()
}

// Sets the cover choice, refusing an image that isn't a readable JPEG or PNG.
pub fn set_cover(plan: &mut Plan, cover: Cover) -> Result<(), String> {
    validate(&cover)?;
    plan.metadata.cover = cover;
    Ok(())
}

// What Whisper heard either side of a break, if it transcribed there.
fn heard(plan: &Plan, i: usize) -> Option<(&str, &str)> {
    let at = pos(plan.findings.breaks.get(i)?);
    let s = plan.findings.silences.iter().find(|s| same((s.file, s.at), at))?;
    Some((s.before.as_str(), s.after.as_str())).filter(|(b, a)| !(b.is_empty() && a.is_empty()))
}

fn io<T, E: std::fmt::Display>(r: Result<T, E>) -> Result<T, String> {
    r.map_err(|e| e.to_string())
}

fn pick(prompt: &str, items: &[String]) -> Result<Option<usize>, String> {
    io(Select::with_theme(&ColorfulTheme::default()).with_prompt(prompt).items(items).default(0).interact_opt())
}

fn confirm(prompt: &str) -> Result<bool, String> {
    io(Confirm::with_theme(&ColorfulTheme::default()).with_prompt(prompt).default(false).interact())
}

fn ask(prompt: &str, initial: &str, allow_empty: bool) -> Result<String, String> {
    io(Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .with_initial_text(initial)
        .allow_empty(allow_empty)
        .interact_text())
}

// A mistake or refusal is shown and the menu carries on; only a broken terminal ends it.
fn report(result: Result<(), String>) {
    if let Err(e) = result {
        println!("{}", e);
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// Runs the editor on `plan`. Returns whether to save it.
pub fn edit_plan(plan: &mut Plan) -> Result<bool, String> {
    let original = (plan.findings.breaks.clone(), plan.metadata.clone());
    let menu = strings(&[
        "Review and edit chapters",
        "Add a chapter at a time",
        "Add a chapter at a silence the analysis skipped",
        "Edit book tags and file naming",
        "Discard all changes",
        "Save and exit",
        "Exit without saving",
    ]);
    loop {
        let missing = plan.findings.missing.len();
        println!("\n{} chapter(s){}", plan.findings.breaks.len(), if missing > 0 { format!(", {} not found", missing) } else { String::new() });
        match pick("What next?", &menu)? {
            Some(0) => chapters_menu(plan)?,
            Some(1) => report(add_at_time(plan)),
            Some(2) => report(add_at_silence(plan)),
            Some(3) => tags_menu(plan)?,
            Some(4) if changed(plan, &original) => {
                if !confirm("Discard all changes?")? {
                    continue;
                }
                (plan.findings.breaks, plan.metadata) = original.clone();
                println!("Changes discarded.");
            }
            Some(5) => return Ok(true),
            Some(6) => {
                if !changed(plan, &original) || confirm("Exit and lose your changes?")? {
                    return Ok(false);
                }
            }
            _ => {} // Esc: stay, so a stray key can't lose the edits.
        }
    }
}

fn chapters_menu(plan: &mut Plan) -> Result<(), String> {
    loop {
        let mut items: Vec<String> = plan
            .findings
            .breaks
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let score = b.score.map_or(String::new(), |s| format!("  (match {:.2})", s));
                format!("{:>3}. {}  {}{}", i + 1, place(plan, b.file, b.at), b.title, score)
            })
            .collect();
        items.push("Back".into());
        match pick("Choose a chapter", &items)? {
            Some(i) if i < plan.findings.breaks.len() => chapter_menu(plan, i)?,
            _ => return Ok(()),
        }
    }
}

fn chapter_menu(plan: &mut Plan, i: usize) -> Result<(), String> {
    let menu = strings(&["Rename", "Move to the previous silence", "Move to the next silence", "Show what Whisper heard", "Remove", "Back"]);
    loop {
        let title = plan.findings.breaks[i].title.clone();
        let result = match pick(&title, &menu)? {
            Some(0) => rename(plan, i, &ask("Title", &title, false)?),
            Some(1) => move_break(plan, i, -1),
            Some(2) => move_break(plan, i, 1),
            Some(3) => {
                match heard(plan, i) {
                    Some((before, after)) => println!("  heard before: {}\n  heard after:  {}", before, after),
                    None => println!("  Nothing was transcribed here (no EPUB, or the break was added by hand)."),
                }
                Ok(())
            }
            Some(4) => match remove(plan, i) {
                Ok(()) => return Ok(()),
                err => err,
            },
            _ => return Ok(()),
        };
        report(result);
        if let Some(b) = plan.findings.breaks.get(i) {
            println!("  now at {}", place(plan, b.file, b.at));
        }
    }
}

fn add_at_time(plan: &mut Plan) -> Result<(), String> {
    let file = if plan.files.len() > 1 {
        let names: Vec<String> = plan.files.iter().map(|f| audiobook_splitter_rust::pipeline::file_name(&f.path)).collect();
        match pick("Which file?", &names)? {
            Some(i) => i,
            None => return Ok(()),
        }
    } else {
        0
    };
    let at = parse_time(&ask("Start time (h:mm:ss)", "", false)?)?;
    let title = ask("Title", "New chapter", false)?;
    add(plan, file, at, &title)
}

fn add_at_silence(plan: &mut Plan) -> Result<(), String> {
    let spots = unused_silences(plan);
    if spots.is_empty() {
        return Err("Every silence already starts a chapter.".into());
    }
    let items: Vec<String> = spots
        .iter()
        .map(|&(file, at)| {
            let after = plan.findings.silences.iter().find(|s| same((s.file, s.at), (file, at))).map_or("", |s| s.after.as_str());
            let preview: String = after.chars().take(50).collect();
            format!("{}  {}", place(plan, file, at), preview)
        })
        .collect();
    let Some(i) = pick("Start a chapter at", &items)? else { return Ok(()) };
    let title = ask("Title", "New chapter", false)?;
    add(plan, spots[i].0, spots[i].1, &title)
}

fn tags_menu(plan: &mut Plan) -> Result<(), String> {
    println!("Leave a tag empty to keep the source file's own tag.");
    let m = &mut plan.metadata;
    m.album = ask("Album", &m.album, true)?;
    m.artist = ask("Artist", &m.artist, true)?;
    m.album_artist = ask("Album artist", &m.album_artist, true)?;
    m.genre = ask("Genre", &m.genre, true)?;
    m.year = ask("Year", &m.year, true)?;
    m.template = ask("File naming: {n} (from 00), {title}, {album}", &m.template, false)?;
    report(cover_menu(plan));
    Ok(())
}

fn cover_menu(plan: &mut Plan) -> Result<(), String> {
    let embedded = find_embedded(&plan.files).map(|(i, _)| file_name(&plan.files[i].path));
    let items = vec![
        embedded.map_or("Keep the embedded cover (none found in the source files)".to_string(), |name| format!("Keep the cover embedded in {}", name)),
        "No cover art".to_string(),
        "Attach a JPEG or PNG image".to_string(),
    ];
    let current = match &plan.metadata.cover {
        Cover::File(path) => path.display().to_string(),
        _ => String::new(),
    };
    match pick("Cover art", &items)? {
        Some(0) => set_cover(plan, Cover::Keep),
        Some(1) => set_cover(plan, Cover::None),
        Some(2) => {
            let typed = ask("Image file", &current, false)?;
            set_cover(plan, Cover::File(user_path(&typed)))
        }
        _ => Ok(()), // Esc: leave the choice as it was.
    }
}

#[derive(clap::Args)]
pub struct Args {
    /// Plan file made by `analyze`; edited in place
    pub plan: PathBuf,
}

pub fn run(args: Args) -> Result<(), String> {
    prompt::require_terminal("edit")?;
    let mut plan = open_plan(&args.plan)?;
    if edit_plan(&mut plan)? {
        plan.save(&args.plan)?;
        println!("Saved {}.", args.plan.display());
    } else {
        println!("Nothing saved.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use audiobook_splitter_rust::chapters::AudioFile;
    use audiobook_splitter_rust::plan::{Findings, PlanSilence, Settings};

    // One 100 s file, chapters at 0, 30 and 60 s, silences at 0, 10, 20, 30, 45, 60 and 80 s.
    fn plan() -> Plan {
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
