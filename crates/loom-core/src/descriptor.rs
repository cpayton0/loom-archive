use serde::{Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceType {
    Mp4,
    Hls,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct AssetHints {
    pub transcript: bool,
    pub captions: bool,
    pub thumbnail: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDescriptor {
    pub id: String,
    pub title: Option<String>,
    /// The title as a safe file name, used when naming files by title.
    pub title_file_name: Option<String>,
    pub duration_seconds: Option<f64>,
    pub source_url: String,
    pub source_type: SourceType,
    pub assets: AssetHints,
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", remote = "Self")]
pub enum AnalyzeError {
    #[error("Enter an HTTPS Loom share link with a 32-character video ID.")]
    InvalidUrl,
    #[error("Loom denied access to this recording's media.")]
    PermissionDenied,
    #[error("Loom requires authentication for this request.")]
    AuthenticationRequired,
    #[error("Loom supplied an invalid or unsupported media destination.")]
    InvalidMediaUrl,
    #[error("This recording is unavailable.")]
    Unavailable,
    #[error("The playback authorization expired. Try again to read a fresh link from the share page.")]
    MediaAuthorizationExpired,
    #[error("Loom asked us to slow down. Try again later.")]
    RateLimited,
    #[error("The share page redirected. Paste the final Loom share link.")]
    Redirect,
    #[error("Could not reach Loom. Check your connection and try again.")]
    Network,
    #[error("Loom returned a page format this version cannot read.")]
    PageFormat,
    #[error("The page did not identify a supported MP4 or HLS source.")]
    NoMedia,
    #[error("The response exceeded the share-page size limit.")]
    PageTooLarge,
}

impl Serialize for AnalyzeError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct WireError {
            #[serde(with = "AnalyzeError")]
            code: AnalyzeError,
            message: String,
        }
        WireError {
            code: *self,
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}
