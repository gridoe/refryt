//! Lucide icons (ISC), registered once as `bytes://` sources and tinted per use.
//!
//! Generated from `assets/icons/*.svg`. egui_extras' SVG loader rasterises at the requested size,
//! so an icon stays crisp at any scale.

use egui::Color32;

macro_rules! icons {
    ($($variant:ident => $file:literal),* $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
        pub enum Icon { $($variant),* }

        const ALL: &[(Icon, &str, &[u8])] = &[$((
            Icon::$variant,
            concat!("bytes://ytm-icon-", $file, ".svg"),
            include_bytes!(concat!("../../../assets/icons/", $file, ".svg")).as_slice(),
        )),*];
    };
}

icons! {
    ArrowUpDown => "arrow-up-down",
    ChevronLeft => "chevron-left",
    ChevronRight => "chevron-right",
    Check => "check",
    Compass => "compass",
    CircleHelp => "circle-help",
    CircleUser => "circle-user",
    Bookmark => "bookmark",
    CircleAlert => "circle-alert",
    ChartColumn => "chart-column",
    Disc3 => "disc-3",
    Download => "download",
    Ellipsis => "ellipsis",
    Folder => "folder",
    Github => "github",
    Heart => "heart",
    HeartFilled => "heart-filled",
    History => "history",
    House => "house",
    Info => "info",
    Library => "library",
    Link => "link",
    ListPlus => "list-plus",
    ListStart => "list-start",
    ListX => "list-x",
    ListMusic => "list-music",
    LoaderCircle => "loader-circle",
    LogOut => "log-out",
    MicVocal => "mic-vocal",
    Maximize => "maximize",
    Maximize2 => "maximize-2",
    Minimize => "minimize",
    Minimize2 => "minimize-2",
    Minus => "minus",
    Music => "music",
    PanelLeft => "panel-left",
    PauseFilled => "pause-filled",
    Pencil => "pencil",
    PlayFilled => "play-filled",
    Plus => "plus",
    Radio => "radio",
    RefreshCw => "refresh-cw",
    Repeat => "repeat",
    Repeat1 => "repeat-1",
    Search => "search",
    Settings => "settings",
    Shuffle => "shuffle",
    Smile => "smile",
    SkipBackFilled => "skip-back-filled",
    SkipForwardFilled => "skip-forward-filled",
    Square => "square",
    ThumbsDown => "thumbs-down",
    ThumbsDownFilled => "thumbs-down-filled",
    ThumbsUp => "thumbs-up",
    ThumbsUpFilled => "thumbs-up-filled",
    Trash2 => "trash-2",
    TrendingDown => "trending-down",
    TrendingUp => "trending-up",
    Video => "video",
    Volume1 => "volume-1",
    Volume2 => "volume-2",
    VolumeX => "volume-x",
    WifiOff => "wifi-off",
    X => "x",
}

impl Icon {
    pub fn uri(self) -> &'static str {
        ALL.iter()
            .find(|(i, _, _)| *i == self)
            .map(|(_, u, _)| *u)
            .unwrap()
    }

    /// An `Image` sized and tinted for immediate use.
    pub fn image(self, color: Color32, size: f32) -> egui::Image<'static> {
        egui::Image::new(self.uri())
            .fit_to_exact_size(egui::vec2(size, size))
            .tint(color)
    }
}

/// Register every icon as an in-memory byte source. Call once at startup.
pub fn register(ctx: &egui::Context) {
    for (_, uri, bytes) in ALL {
        ctx.include_bytes(*uri, *bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_has_a_unique_uri() {
        let mut uris: Vec<&str> = ALL.iter().map(|(_, u, _)| *u).collect();
        let total = uris.len();
        uris.sort_unstable();
        uris.dedup();
        assert_eq!(uris.len(), total, "duplicate icon uri");
    }

    #[test]
    fn every_icon_carries_svg_bytes() {
        for (icon, _, bytes) in ALL {
            assert!(!bytes.is_empty(), "{icon:?} is empty");
            assert!(
                bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml"),
                "{icon:?} does not look like an SVG"
            );
        }
    }
}
