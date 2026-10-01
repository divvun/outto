//! Manual smoke test for the Authorization Services + SMJobSubmit elevation
//! path: shows the admin prompt (Touch ID where available), runs `id` as root
//! in a launchd job, and prints what it streamed back.
//!
//! cargo +nightly run -p outto-macos --example elevate

fn main() {
    use std::ffi::OsString;

    use outto_macos::elevation::{AuthPrompt, StreamEvent, run_elevated_with_progress};

    let tmp = tempfile::Builder::new()
        .prefix("outto-elevate-example")
        .tempdir()
        .unwrap();
    let progress = tmp.path().join("progress.jsonl");
    let script = r#"printf '{"type":"log","level":"info","message":"%s"}\n{"type":"finished","ok":true}\n' "$(id)" >> "$1""#;
    let argv: Vec<OsString> = vec![
        "-c".into(),
        script.into(),
        "sh".into(),
        progress.clone().into(),
    ];
    let prompt = AuthPrompt::install("no.divvun.outto.example", "the outto elevation example");

    let outcome = run_elevated_with_progress(
        std::path::Path::new("/bin/sh"),
        &argv,
        &progress,
        &prompt,
        |ev| match ev {
            StreamEvent::Log { message, .. } => println!("child: {message}"),
            StreamEvent::Progress { .. } => {}
        },
    );
    println!("outcome: {outcome:?}");
}
