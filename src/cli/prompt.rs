// Terminal checks and the few prompts the guided `run` needs.
use audiobook_splitter_rust::{collect_inputs, user_path};
use dialoguer::{theme::ColorfulTheme, Input};
use std::io::IsTerminal;
use std::path::PathBuf;

pub fn is_terminal() -> bool {
    std::io::stdin().is_terminal()
}

pub fn require_terminal(command: &str) -> Result<(), String> {
    if is_terminal() {
        Ok(())
    } else {
        Err(format!(
            "`{}` needs an interactive terminal. Edit the plan file (plain JSON) instead, or run `split` on it as it is.",
            command
        ))
    }
}

fn input(prompt: &str, allow_empty: bool, check: fn(&String) -> Result<(), String>) -> Result<String, String> {
    Input::<String>::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .allow_empty(allow_empty)
        .validate_with(check)
        .interact_text()
        .map_err(|e| e.to_string())
}

pub fn ask_input_path() -> Result<PathBuf, String> {
    require_terminal("run")?;
    let typed = input("Audio file, or a folder of audio files", false, |s| match collect_inputs(&user_path(s)) {
        Ok(files) if !files.is_empty() => Ok(()),
        Ok(_) => Err("No supported audio files in that folder.".to_string()),
        Err(e) => Err(e),
    })?;
    Ok(user_path(&typed))
}

pub fn ask_output_dir() -> Result<PathBuf, String> {
    require_terminal("run")?;
    Ok(user_path(&input("Output folder (created if needed)", false, |_| Ok(()))?))
}

pub fn ask_optional_epub() -> Result<Option<PathBuf>, String> {
    require_terminal("run")?;
    let typed = input("Book EPUB to match chapters with AI (Enter to skip)", true, |s| {
        if s.trim().is_empty() || user_path(s).is_file() { Ok(()) } else { Err("No such file.".to_string()) }
    })?;
    Ok(Some(user_path(&typed)).filter(|p| !p.as_os_str().is_empty()))
}
