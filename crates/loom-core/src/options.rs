use serde::{Deserialize, Serialize};

/// How saved files are named. Mirrors upstream's default (ID) and `--use-title`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileNaming {
    #[default]
    Id,
    Title,
}

/// What the save produces. `TranscriptOnly` mirrors upstream's `--transcript-only`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Content {
    #[default]
    Video,
    AudioOnly,
    TranscriptOnly,
}

/// What to do when a planned file name is already taken.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Collision {
    /// Stop before downloading anything.
    #[default]
    Stop,
    /// Use the first free `name (2)`, `name (3)`, … for every file in the set.
    Rename,
}

/// Video quality preference, using upstream's `--quality` choices.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Quality {
    /// Hand Loom's playlist to FFmpeg unchanged, exactly like upstream.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "best")]
    Best,
    #[serde(rename = "1080p")]
    P1080,
    #[serde(rename = "720p")]
    P720,
    #[serde(rename = "480p")]
    P480,
}

impl Quality {
    pub(crate) fn max_height(self) -> Option<u64> {
        match self {
            Self::Auto | Self::Best => None,
            Self::P1080 => Some(1080),
            Self::P720 => Some(720),
            Self::P480 => Some(480),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SaveOptions {
    pub naming: FileNaming,
    pub content: Content,
    pub quality: Quality,
    /// Also save `<name>.transcript.json` (upstream `--transcript`).
    pub transcript: bool,
    /// Also save `<name>-thumbnail.jpg` (upstream `--image`).
    pub thumbnail: bool,
    /// Also save `<name>.captions.vtt`. Upstream lists captions but never saves them.
    pub captions: bool,
    /// Also save `<name>-thumbnail.gif` (upstream `--gif`).
    pub gif: bool,
    /// Also save `<name>-seek-preview.jpg` and `.vtt` (upstream `--seek-preview`).
    pub seek_preview: bool,
    pub collision: Collision,
}

/// Upstream `sanitizeFilename`, plus Windows reserved-name and trailing-dot handling.
pub(crate) fn sanitize_filename(name: &str) -> Option<String> {
    let replaced: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed: String = collapsed.chars().take(200).collect();
    let trimmed = trimmed.trim_end_matches(['.', ' ']).trim();
    let stem = trimmed.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit());
    (!trimmed.is_empty() && !reserved).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_become_safe_file_names() {
        assert_eq!(
            sanitize_filename("Q3: plan / review?  \"draft\"").as_deref(),
            Some("Q3- plan - review- -draft-")
        );
        assert_eq!(sanitize_filename("  notes...  ").as_deref(), Some("notes"));
        assert_eq!(sanitize_filename("CON"), None);
        assert_eq!(sanitize_filename("com1.txt"), None);
        assert_eq!(sanitize_filename("..."), None);
        assert_eq!(sanitize_filename(&"a".repeat(300)).unwrap().len(), 200);
    }

    #[test]
    fn options_use_upstream_names_on_the_wire() {
        let options: SaveOptions =
            serde_json::from_str(r#"{"quality":"720p","content":"audioOnly"}"#).unwrap();
        assert_eq!(options.quality, Quality::P720);
        assert_eq!(options.content, Content::AudioOnly);
        assert_eq!(options.naming, FileNaming::Id);
    }
}
