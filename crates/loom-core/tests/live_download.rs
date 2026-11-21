//! Opt in with LOOM_TEST_SHARE_URL. HLS recordings need FFmpeg on PATH.
use loom_core::{CancellationToken, SaveOptions, SaveRequest, analyze_url, save_recording};

#[tokio::test]
#[ignore = "requires a Loom share URL, network and FFmpeg"]
async fn live_share_download() {
    let url = std::env::var("LOOM_TEST_SHARE_URL").expect("set LOOM_TEST_SHARE_URL");
    let recording = analyze_url(&url).await.expect("analyze the share page");
    println!(
        "Source {:?}; title present {}",
        recording.source_type,
        recording.title.is_some(),
    );
    let folder = tempfile::tempdir().unwrap();
    let request = SaveRequest {
        options: SaveOptions {
            transcript: true,
            thumbnail: true,
            ..SaveOptions::default()
        },
        retries: 1,
        diagnostics: Some(loom_core::Diagnostics::default()),
        ..SaveRequest::default()
    };
    let saved = save_recording(
        &url,
        folder.path(),
        &request,
        &CancellationToken::new(),
        |_| {},
    )
    .await
    .expect("save and verify the recording");
    for file in &saved.files {
        println!("{}: {} bytes", file.display(), file.metadata().unwrap().len());
    }
    println!("Notes: {:?}", saved.notes);
}
