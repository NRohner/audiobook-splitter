// `edit`'s prompt-based editor: the menus only choose which `ops` edit to call.
use super::ops::{add, changed, heard, move_break, remove, rename, same, set_cover, unused_silences};
use super::super::time::parse_time;
use super::super::place;
use audiobook_splitter_rust::cover::find_embedded;
use audiobook_splitter_rust::meta::Cover;
use audiobook_splitter_rust::pipeline::file_name;
use audiobook_splitter_rust::plan::Plan;
use audiobook_splitter_rust::user_path;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

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
