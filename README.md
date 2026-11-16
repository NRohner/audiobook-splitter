# Rust Audio Splitter

---

A Rust application (local web GUI plus a full command-line interface that works over SSH) that intelligently splits long audio files (MP3, M4B, WAV, FLAC, AAC, M4A or OGG) into smaller segments. It achieves this by scanning the audio for periods of silence that exceed a user-defined threshold and then splitting the file at the midpoint of these detected silences, preventing abrupt audio clipping.

## Features

* **Silence Detection:** Accurately identifies sections of silence within audio files.

* **Customizable Thresholds:** Users can specify the minimum **silence duration** and **noise threshold (in dB)** to fine-tune detection sensitivity.

* **Re-analysis:** Re-analyze the audio with different settings in the GUI, or with `analyze` (and `rescan` for chapters the EPUB match missed) in the CLI.

* **Mid-Silence Splitting:** Splits audio precisely in the middle of a detected silence to ensure smooth transitions between segments.

* **Status Updates:** Provides regular progress updates during the detection and splitting process.

* **AI Chapter Matching (optional):** Give it the book's EPUB and a local Whisper speech-to-text model transcribes ~20 seconds either side of each silence. The app splits only where the narration matches the start of an EPUB chapter, and names the files after the chapters (e.g. `03 - The Storm.mp3`). In the GUI you can play 30 seconds from each matched break to check it by ear. Everything runs offline on your machine.

* **Chapter Editor (GUI):** After analysis, rename, remove or add chapters, move a break to the previous or next silence, add one at any time (`h:mm:ss`), or promote a silence the analysis skipped. Every break can be played back to check it by ear. Also available in the CLI as `edit`.

* **Terminal app (TUI):** `cargo run -- tui` (or the release binary's `tui` command) is a full-screen version of the whole workflow: choose the book with a built-in file browser, set the analysis options, watch the progress, edit chapters, tags and cover art, and split. It works over SSH and in tmux and needs a terminal of at least 60x15.

* **Metadata Editor (GUI):** Set the album, artist, album artist, genre and year for the whole book, and a track naming convention such as `{n} - {title}` (`{n}` counts from 00; `{album}` is also available). Every output file is tagged disc 1 with sequential track numbers across the whole book. Empty fields keep the source file's tags. Cover art: keep the cover already embedded in the source (the default), attach a JPEG or PNG, or write none. It is written to every MP3 and M4A/M4B output.

* **Multi-file Processing:** Option to process multiple audio files consecutively.

## Setup

You need five things on any platform:

* **Rust** (with Cargo): <https://www.rust-lang.org/tools/install>
* **CMake** and a **C/C++ compiler**: the Whisper speech-to-text engine (whisper.cpp) is compiled into the app.
* **Clang / libclang**: the Whisper bindings use it at build time to generate their Rust interface (the Xcode command line tools include it on macOS; Linux and Windows install it separately, see below).
* **FFmpeg and FFprobe** on your `PATH`: the app uses them to analyze and split audio.
* The **Whisper model** (`models/ggml-base.en.bin`, ~148 MB): the app downloads it with `curl` the first time it is needed (or run `make model` up front); see below.

Windows also needs the **Vulkan SDK**, so Whisper can run on the GPU; on Linux the Vulkan development packages do the same job (see below).

### macOS

1.  **Install the Xcode command line tools** (C/C++ compiler and `make`):

    ```bash
    xcode-select --install
    ```

2.  **Install Rust:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

3.  **Install CMake and FFmpeg** with [Homebrew](https://brew.sh) (FFprobe ships with FFmpeg):

    ```bash
    brew install cmake ffmpeg
    ```

4.  **Clone and run:**

    ```bash
    git clone https://github.com/NRohner/audiobook-splitter.git
    cd audiobook-splitter
    make run     # downloads the Whisper model on first run, builds the release binary, opens the GUI
    ```

On Apple Silicon, Whisper runs on the GPU via Metal.

### Linux

1.  **Install the build tools** (C/C++ compiler, CMake, and Clang, whose headers the Whisper bindings need at build time), the Vulkan development files and shader compiler (`libvulkan-dev`, `glslc`) plus a Vulkan driver (`mesa-vulkan-drivers` covers AMD and Intel; NVIDIA's proprietary driver includes its own), and FFmpeg. On Debian/Ubuntu:

    ```bash
    sudo apt install build-essential cmake clang libvulkan-dev glslc mesa-vulkan-drivers ffmpeg curl git
    ```

    Fedora: `sudo dnf install gcc-c++ cmake clang vulkan-loader-devel glslc mesa-vulkan-drivers ffmpeg`. Arch: `sudo pacman -S base-devel cmake clang vulkan-headers vulkan-icd-loader shaderc vulkan-radeon ffmpeg` (`vulkan-intel` or `nvidia-utils` for those GPUs). (Fedora needs the RPM Fusion repo for the full `ffmpeg`.)

2.  **Install Rust:**

    ```bash
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    ```

3.  **Clone and run:**

    ```bash
    git clone https://github.com/NRohner/audiobook-splitter.git
    cd audiobook-splitter
    make run     # downloads the Whisper model on first run, builds the release binary, opens the GUI
    ```

Whisper runs on the GPU through Vulkan when a usable one is found. At start-up the app checks which GPU it would get and falls back to the CPU if there is none, only software Vulkan (llvmpipe), or a GPU whisper.cpp can't drive (such as Intel Haswell graphics). Set `ABS_VULKAN=0` to force the CPU, or `ABS_VULKAN=1` to skip the check. The native **File** / **Folder** / **Browse** dialogs use the desktop's file-chooser portal (`xdg-desktop-portal` plus your desktop's backend, which GNOME, KDE and most others ship); if there is none, type the path instead. The GUI opens your browser with `xdg-open`.

### Windows

1.  **Install Rust:** download and run `rustup-init.exe` from <https://www.rust-lang.org/tools/install>. When asked, let it install the **Visual Studio C++ Build Tools** (the "Desktop development with C++" workload); that is the C/C++ compiler.

2.  **Install CMake, Git, LLVM and the Vulkan SDK** (LLVM provides `libclang.dll`, which the Whisper bindings need at build time; the Vulkan SDK lets Whisper run on any NVIDIA, AMD or Intel GPU):

    ```powershell
    winget install Kitware.CMake Git.Git LLVM.LLVM KhronosGroup.VulkanSDK
    [Environment]::SetEnvironmentVariable("LIBCLANG_PATH", "C:\Program Files\LLVM\bin", "User")
    [Environment]::SetEnvironmentVariable("CARGO_TARGET_DIR", "C:\abs-target", "User")
    ```

    The Vulkan SDK installer sets `VULKAN_SDK` for you. `CARGO_TARGET_DIR` moves the build folder to a short path: whisper.cpp's GPU code builds files nested so deep that, under the repo's `target\` folder, they pass Windows' 260-character path limit and the build fails. The built binary is then `C:\abs-target\release\audiobook-splitter-rust.exe`; run it (or `cargo run -- gui`) from the repo folder so it finds `models\`.

    Nothing GPU-specific is needed to *run* the app: your graphics driver already includes Vulkan. Without a usable GPU, Whisper falls back to the CPU (roughly 50 times slower).

3.  **Install FFmpeg and FFprobe.** The quickest way is winget, which also adds them to your `PATH`:

    ```powershell
    winget install Gyan.FFmpeg
    ```

    <details>
    <summary>Manual install instead</summary>

    1.  Download `ffmpeg-release-essentials.7z` (or `ffmpeg-release-full.7z` for more codecs) from <https://www.gyan.dev/ffmpeg/builds/>.
    2.  Extract it with 7-Zip, rename the extracted folder to `FFmpeg` and move it to `C:\FFmpeg`.
    3.  Search the Start menu for "environment variables", open **Edit the system environment variables**, click **Environment Variables...**, select `Path` under **System variables**, click **Edit...**, then **New**, and add `C:\FFmpeg\bin`.
    4.  Click **OK** on every window.

    </details>

4.  **Check the tools:** open a **new** PowerShell window (`PATH` changes don't reach windows that were already open) and run:

    ```powershell
    cargo --version
    cmake --version
    Test-Path "$env:LIBCLANG_PATH\libclang.dll"   # should print True
    Test-Path "$env:VULKAN_SDK\Lib\vulkan-1.lib"  # should print True
    ffmpeg -version
    ffprobe -version
    ```

5.  **Clone, download the model and run.** The Makefile needs `make`, which Windows doesn't include, so run the equivalent commands directly:

    ```powershell
    git clone https://github.com/NRohner/audiobook-splitter.git
    cd audiobook-splitter
    mkdir models
    curl.exe -L --fail -o models\ggml-base.en.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
    cargo run --release -- gui   # builds the release binary, opens the GUI
    ```

    Use `curl.exe`, not `curl`: in Windows PowerShell 5, `curl` is an alias for `Invoke-WebRequest`, which takes different arguments.

## Running

| macOS / Linux | Windows (PowerShell) | What it does |
|---|---|---|
| `make model` | see step 5 above | One-time: download the Whisper model into `models/` |
| `make dev` | `cargo run -- gui` | Debug build, starts the GUI and opens your browser |
| `make build` | `cargo build --release` | Optimized single binary at `target/release/audiobook-splitter-rust` (`C:\abs-target\release\audiobook-splitter-rust.exe` on Windows) |
| `make run` | `cargo run --release -- gui` | Build and launch the release GUI |
| `make test` | `cargo test` | Run the test suite |
| `make cli` | `cargo run -- run` | Guided terminal session |

The GUI is a local web page served by the app itself at `http://127.0.0.1:7878` (it only listens on your machine). Pick an audio file or folder and an output folder (type a path or use the native **File** / **Folder** / **Browse** dialogs), set the detection thresholds, click **Analyze**, tweak and re-analyze until the segment counts look right, then click **Split files**.

The binary is self-contained (the page is compiled in); FFmpeg/FFprobe still need to be on your `PATH`. Set `WHISPER_MODEL=/path/to/ggml-*.bin` to use a different model (e.g. `ggml-small.en.bin` for higher accuracy). In PowerShell: `$env:WHISPER_MODEL = "C:\path\to\ggml-small.en.bin"`.

### Matching chapters with an EPUB

1. Fill in **Book EPUB** (the same edition as the audiobook works best; its table of contents should mark the chapters).
2. Use a *looser* minimum silence than usual (e.g. 1.0-2.0 s): silences are only candidates now, and the transcript match decides which are real chapter breaks.
3. Click **Analyze**. The results list each chapter break with a score (0-2: how well the speech after the pause matches the chapter's opening, plus how well the speech before it matches the previous chapter's ending) and what Whisper heard. Chapters with no match are listed; front matter like the copyright page usually isn't narrated, so that's expected.
4. If real chapters are missing, lower the minimum silence or the **Chapter match threshold** (default 0.4) and analyze again.
5. **Split files** writes one file per chapter. A chapter that crosses an audio file boundary becomes `NN - Title (part 2)`, and anything before the first chapter is `00 - Opening`. The CLI does the same with `analyze --epub`.

## Usage (CLI)

The CLI does everything the GUI does, step by step, joined by a plan file. It works over SSH: nothing needs a display. Run `audiobook-splitter-rust --help` (or `cargo run -- --help`) for the commands, and `<command> --help` for each one's flags.

| Command | What it does |
|---|---|
| `analyze <input>` | Detect silences (and match EPUB chapters with `--epub`), write `plan.json` |
| `rescan <plan>` | Look again for chapters the EPUB match missed (`--chapter N` for one) |
| `edit <plan>` | Full-screen chapter editor: table of chapters, what Whisper heard around each break, single-key actions with undo/redo, tags and cover form. `--simple` gives the plain prompt menu instead |
| `preview <plan> <n>` | Write 30 s from break *n* to an MP3, to check by ear (`scp` it, or `--play` locally) |
| `split <plan> -o <dir>` | Write the tagged files (`--album`, `--artist`, `--template`, `--cover keep\|none\|PATH`, ...) |
| `run <input> -o <dir>` | Analyze, edit and split in one go (`--yes` skips the editor, `--simple` uses the plain prompt menu instead of the full-screen editor) |
| `tui [PATH]` | Full-screen app for the whole workflow. `PATH` may be an audio file or folder (starts at Setup) or a plan file (opens the editor) |
| `cache info` / `cache clear [path ...]` | Show or empty the cache of silences and transcripts (see below) |
| `gui` | Start the web GUI |

A typical remote session:

```bash
audiobook-splitter-rust analyze ~/books/mybook/ --epub mybook.epub --min-silence 1.5 -o mybook.plan.json
audiobook-splitter-rust edit mybook.plan.json
audiobook-splitter-rust split mybook.plan.json -o ~/books/mybook-split
```

Transcription is the slow step; the plan file means you do it once and can edit and split later (run it in `tmux` so a dropped connection doesn't lose it). A plan records the audio files it was made from and is rejected if they change. `run` with no arguments (or the old `--cli` flag) asks for the input, output and an optional EPUB. Running the binary with no arguments prints the help; the GUI needs `gui`.

### Terminal app

`tui` walks through Setup, Analyze, Editor, Split and Done in one full-screen app. Give it an audio file or folder to start at Setup with the input filled in, or a `plan.json` to open the editor directly (it asks for the output folder before splitting). After analysis the plan is saved as `<output>/plan.json`. `b` in the editor goes back to Setup; on Done, `n` starts another book and `q` quits.

It needs a real terminal of at least 60x15 with `TERM` set, and has no menu fallback. `edit` and `run` use the same editor and fall back to the plain menu with `--simple`, on a dumb or too-small terminal, when stdin or stdout is not a terminal, or if the terminal can't be set up. Not included: mouse support, playback, and cancelling a running job.

Editor keys (`?` shows them in the app):

| Key | Action |
|---|---|
| Up/Down, `j`/`k`, PgUp/PgDn, Home/End | Move the selection |
| Enter, `r` | Rename the chapter |
| `[` `]` | Move the break to the previous or next silence |
| `d` | Remove the chapter |
| `a` | Add a chapter at a time |
| `n` | Add a chapter at a silence |
| `m` | List the chapters the EPUB match missed (the table title shows how many); in the list Enter picks a silence to place one by hand, `r` re-scans the highlighted chapter, `a` re-scans all of them, and `m` or Esc closes the list |
| `R` | Re-scan all missing chapters (`tui` only, needs the plan's EPUB) |
| `t` | Tags, file naming and cover art |
| `g` | Jump to a time (`1:02:03`) or a chapter number |
| `p` | Write a preview clip of the selected break |
| `u`, Ctrl-r | Undo, redo |
| `s` | Save |
| `q`, Ctrl-c | Quit (asks if there are unsaved changes) |
| `b` | Back to Setup (`tui` only) |
| `?` | Help |

A re-scan searches only the stretch between the nearest found chapters either side of each missing chapter, with a shorter minimum silence and a higher noise threshold, and runs one chapter at a time on the progress screen. It is one undo step, selects the first chapter it found and needs Whisper, like `rescan`.

Cover art is chosen in the tags form: keep the embedded cover, use none, or use an image (Ctrl-B on the image row opens the file browser for a JPEG or PNG).

File browser keys: Enter opens a folder or picks a file, Space picks the current folder (where a folder is wanted), Backspace goes up, `/` filters the list, `.` shows or hides hidden files, Tab types a path, `~` jumps to the home folder, Esc cancels.

### Resuming and the cache

Silence detection and Whisper transcription are cached on disk, keyed by each audio file (path, size, modified time) and the model, so an interrupted `analyze` picks up where it stopped (at most the last 64 transcriptions are redone), and re-analyzing with a different `--threshold` or re-running `rescan` reuses everything already transcribed. The GUI uses the same cache.

```bash
audiobook-splitter-rust cache info                  # where it is and how big
audiobook-splitter-rust cache clear                 # delete everything
audiobook-splitter-rust cache clear ~/books/mybook  # delete just this book's entries
audiobook-splitter-rust analyze book/ --no-cache    # ignore the cache for one run
```

The folder is `~/Library/Caches/audiobook-splitter` (macOS), `$XDG_CACHE_HOME/audiobook-splitter` (default `~/.cache/audiobook-splitter`) (Linux) or `%LOCALAPPDATA%\audiobook-splitter\cache` (Windows); set `AUDIOBOOK_CACHE_DIR` to put it elsewhere. Entries never expire: a changed audio file or a different model simply doesn't match, so `cache clear` is only for reclaiming space. rescan's narrower silence passes are not cached, only their transcriptions.

## Troubleshooting and Tips

* **"No silences detected":** If you're getting this message even with seemingly quiet audio, try adjusting the **noise threshold (n value)**. Experiment with less negative values like `-30.0dB`, `-20.0dB`, or even `-10.0dB`. The optimal value depends on the specific audio's background noise.

* **"FFmpeg/FFprobe not found":** Ensure FFmpeg and FFprobe are correctly installed and their `bin` directory is added to your system's PATH environment variable. On macOS, `brew install ffmpeg` handles this. On Windows, remember to open a *new* terminal window after modifying the PATH.

* **Abrupt Splitting:** The application is designed to split in the middle of silence, but for very short silences or specific audio content, you might still perceive a subtle cut. Adjusting the `silence length threshold` can help.