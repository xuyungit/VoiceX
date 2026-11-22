use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn configure_protoc() {
    println!("cargo:rerun-if-env-changed=PROTOC");
    if env::var_os("PROTOC").is_some() {
        return;
    }

    let protoc_path =
        protoc_bin_vendored::protoc_bin_path().expect("Failed to locate vendored protoc");
    env::set_var("PROTOC", protoc_path);
}

fn run_tauri_build() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        // `tauri_build`'s default winres manifest only attaches to `[[bin]]` targets,
        // leaving `cargo test --lib` without Common Controls v6 (`TaskDialogIndirect`
        // fails at startup with 0xc0000139). Let the MSVC linker embed it for all
        // linked targets (bins and test harnesses) instead of duplicating the resource.
        let attrs = tauri_build::Attributes::new().windows_attributes(
            tauri_build::WindowsAttributes::new_without_app_manifest(),
        );
        tauri_build::try_build(attrs).expect("Failed to run tauri-build");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!(
            "cargo:rustc-link-arg=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
        );
    } else {
        tauri_build::build();
    }
}

fn main() {
    run_tauri_build();

    configure_protoc();

    let profile = env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=VOICEX_BUILD_PROFILE={}", profile);

    let build_time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=VOICEX_BUILD_TIME={}", build_time);

    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=VOICEX_GIT_COMMIT={}", commit);

    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    // Compile Google Cloud Speech-to-Text V2 proto files
    tonic_build::configure()
        .build_server(false)
        .compile_protos(
            &[
                "proto/google/cloud/speech/v2/cloud_speech.proto",
                "proto/google/longrunning/operations.proto",
                "proto/google/rpc/status.proto",
            ],
            &["proto"],
        )
        .expect("Failed to compile Google Speech V2 protos");

    // Strip doc comments from generated proto files to prevent doctest failures.
    // Generated proto comments contain code snippets (e.g. RPC definitions) that
    // are not valid Rust and fail as doctests.
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    for name in &[
        "google.longrunning.rs",
        "google.rpc.rs",
        "google.cloud.speech.v2.rs",
    ] {
        let path = out_dir.join(name);
        if path.exists() {
            let content = fs::read_to_string(&path).unwrap();
            let stripped: String = content
                .lines()
                .filter(|line| !line.trim_start().starts_with("///"))
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(&path, stripped).unwrap();
        }
    }

    println!("cargo:rerun-if-changed=proto/");
}
