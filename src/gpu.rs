// Linux builds include whisper.cpp's Vulkan backend, which only helps on a real GPU:
// software Vulkan (llvmpipe) is far slower than the CPU, and a GPU ggml can't drive
// (e.g. Haswell's iGPU, no 16-bit storage) aborts the process while the backend loads.
// So the app asks a child process which GPU it would get and, if it isn't a usable one,
// hides Vulkan from this process so Whisper runs on the CPU.
// `ABS_VULKAN=1` skips the check and `ABS_VULKAN=0` always uses the CPU.
use std::ffi::CStr;
use std::process::{Command, Stdio};

const PROBE_VAR: &str = "ABS_GPU_PROBE";

// True when this process was started as the probe (see `probe`).
pub fn is_probe() -> bool {
    std::env::var_os(PROBE_VAR).is_some()
}

// The probe: prints the description of each GPU ggml can start a backend on, one per
// line. Starting it may abort, which is what the parent is checking for.
pub fn probe() {
    use whisper_rs_sys::*;
    // SAFETY: read-only queries of ggml's device registry, indices in range; the
    // description is a NUL-terminated string owned by ggml.
    unsafe {
        for i in 0..ggml_backend_dev_count() {
            let dev = ggml_backend_dev_get(i);
            let kind = ggml_backend_dev_type(dev);
            if kind == ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_GPU || kind == ggml_backend_dev_type_GGML_BACKEND_DEVICE_TYPE_IGPU {
                // Creating the backend is what aborts on a GPU ggml can't drive.
                let backend = ggml_backend_dev_init(dev, std::ptr::null());
                if !backend.is_null() {
                    ggml_backend_free(backend);
                    println!("{}", CStr::from_ptr(ggml_backend_dev_description(dev)).to_string_lossy());
                }
            }
        }
    }
}

// Call first thing in `main`, before any other thread exists. Does nothing off Linux,
// where the GPU backends (Metal, Vulkan on Windows) are unchanged.
pub fn guard_vulkan() {
    #[cfg(target_os = "linux")]
    match std::env::var("ABS_VULKAN").as_deref() {
        Ok("1") => {}
        Ok("0") => hide_vulkan(),
        _ => {
            if !usable_gpu() {
                hide_vulkan();
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn usable_gpu() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let output = Command::new(exe).env(PROBE_VAR, "1").stdin(Stdio::null()).stderr(Stdio::null()).output();
    output.is_ok_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).lines().any(|d| !is_software(d)))
}

#[cfg(target_os = "linux")]
fn hide_vulkan() {
    // SAFETY: called from the start of `main`, before any thread is spawned.
    unsafe {
        std::env::set_var("VK_DRIVER_FILES", "/nonexistent");
        std::env::set_var("VK_ICD_FILENAMES", "/nonexistent"); // Older Vulkan loaders.
    }
}

// CPU-emulated Vulkan devices: llvmpipe, lavapipe, SwiftShader.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_software(description: &str) -> bool {
    let d = description.to_lowercase();
    ["llvmpipe", "lavapipe", "swiftshader", "software"].iter().any(|s| d.contains(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_software_vulkan() {
        assert!(is_software("llvmpipe (LLVM 21.1.8, 256 bits)"));
        assert!(is_software("SwiftShader Device (Subzero)"));
        assert!(!is_software("AMD Radeon R7 200 Series (RADV OLAND)"));
        assert!(!is_software("NVIDIA GeForce RTX 3060"));
    }
}
