//! What changed in each release, for What's new in Settings → About and the window shown once
//! after an update.
//!
//! The notes are `release-notes/<version>.md`, built in by `build.rs`, so they read the same
//! offline and in the Flatpak. Only the list under "What changed since …" is shown: the rest of
//! a file is about downloading the release, which an installed copy is past.

include!(concat!(env!("OUT_DIR"), "/changelog.rs"));

/// One change: a bold lead, where the bullet starts with one, and the rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub title: Option<String>,
    pub text: String,
}

/// A release and its changes.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: &'static str,
    pub changes: Vec<Change>,
}

/// Every release up to this one with something to say, newest first. A notes file for a newer
/// version, in a tree being worked on, is left out.
pub fn releases() -> Vec<Release> {
    let current = env!("CARGO_PKG_VERSION");
    NOTES
        .iter()
        .filter(|(version, _)| !crate::update::newer(version, current))
        .map(|(version, notes)| Release {
            version,
            changes: changes(notes),
        })
        .filter(|release| !release.changes.is_empty())
        .collect()
}

/// This version's release, if its notes say what changed.
pub fn this_version() -> Option<Release> {
    releases()
        .into_iter()
        .find(|release| release.version == env!("CARGO_PKG_VERSION"))
}

/// Whether What's new should open at this start: an update, from a version that said what it
/// had last shown (or from one before that was kept, which reads as `None`).
pub fn show_after_update(seen: Option<&str>) -> bool {
    let current = env!("CARGO_PKG_VERSION");
    seen.is_none_or(|seen| crate::update::newer(current, seen))
}

/// The bullets under "What changed since …", with comments taken out and each bullet's wrapped
/// lines joined into one.
fn changes(notes: &str) -> Vec<Change> {
    let Some(start) = notes.find("What changed since") else {
        return Vec::new();
    };
    let list = without_comments(&notes[start..]);
    let mut bullets: Vec<String> = Vec::new();
    let mut open = false;
    for line in list.lines().skip(1) {
        if let Some(rest) = line.strip_prefix("- ") {
            bullets.push(rest.trim().to_owned());
            open = true;
        } else if open && line.starts_with(' ') && !line.trim().is_empty() {
            let last = bullets.last_mut().expect("open means a bullet");
            last.push(' ');
            last.push_str(line.trim());
        } else {
            open = false;
        }
    }
    bullets
        .iter()
        .map(|bullet| split(&without_issue_numbers(bullet)))
        .collect()
}

fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => rest = &rest[start + end + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The notes link issues and pull requests as "(#23)", which mean nothing inside the app. They
/// come out; a credit beside one stays: "(#25, by someone)" reads "(by someone)".
fn without_issue_numbers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("(#") {
        let inner = &rest[open + 2..];
        let digits = inner.len() - inner.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let after = &inner[digits..];
        if digits == 0 {
            out.push_str(&rest[..open + 2]);
            rest = inner;
        } else if let Some(tail) = after.strip_prefix(')') {
            out.push_str(rest[..open].trim_end());
            rest = tail;
        } else if let Some(credit) = after.strip_prefix(", ") {
            out.push_str(&rest[..open]);
            out.push('(');
            rest = credit;
        } else {
            out.push_str(&rest[..open + 2]);
            rest = inner;
        }
    }
    out.push_str(rest);
    out
}

/// Its first letter as a capital: a bullet's rest is written to follow its lead and a dash.
fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// "**Lead** — the rest" into its lead and the rest.
fn split(bullet: &str) -> Change {
    let lead = bullet
        .strip_prefix("**")
        .and_then(|rest| rest.split_once("**"));
    let Some((title, rest)) = lead else {
        return Change {
            title: None,
            text: bullet.to_owned(),
        };
    };
    let rest = rest.trim_start();
    let rest = ["— ", "– ", "- ", ": "]
        .iter()
        .find_map(|sep| rest.strip_prefix(sep))
        .unwrap_or(rest);
    Change {
        title: Some(title.trim_end_matches([':', '.']).to_owned()),
        text: capitalised(rest.trim()),
    }
}

/// A run of text and how to set it: plain, `**bold**` or `` `code` ``.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    Bold,
    Code,
}

/// The text split into its plainly set, bold and code parts. Markers without a partner are left
/// as they are.
pub fn spans(text: &str) -> Vec<(Style, &str)> {
    let mut out = Vec::new();
    let mut rest = text;
    loop {
        let bold = rest.find("**");
        let code = rest.find('`');
        let (at, marker, style) = match (bold, code) {
            (Some(b), Some(c)) if c < b => (c, "`", Style::Code),
            (Some(b), _) => (b, "**", Style::Bold),
            (None, Some(c)) => (c, "`", Style::Code),
            (None, None) => break,
        };
        let inner = &rest[at + marker.len()..];
        let Some(end) = inner.find(marker) else {
            break;
        };
        if at > 0 {
            out.push((Style::Plain, &rest[..at]));
        }
        out.push((style, &inner[..end]));
        rest = &inner[end + marker.len()..];
    }
    if !rest.is_empty() {
        out.push((Style::Plain, rest));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = "How to install.\n\n**Windows** — not a change.\n\nWhat changed since 0.1.0:\n\n\
        - **Stats** — a page of\n  what you played.\n- **Cache:** songs kept.\n- A plain line.\n\n\
        Thanks to everyone.\n";

    #[test]
    fn only_the_list_under_what_changed_is_read() {
        let changes = changes(NOTES);
        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].title.as_deref(), Some("Stats"));
        assert_eq!(changes[0].text, "A page of what you played.");
        assert_eq!(changes[1].title.as_deref(), Some("Cache"));
        assert_eq!(changes[1].text, "Songs kept.");
        assert_eq!(changes[2].title, None);
        assert_eq!(changes[2].text, "A plain line.");
    }

    #[test]
    fn issue_numbers_come_out_and_credits_stay() {
        assert_eq!(
            without_issue_numbers("turns both off (#22). Then (#25, by 9M2PJU)."),
            "turns both off. Then (by 9M2PJU)."
        );
        assert_eq!(without_issue_numbers("a (#tag) stays"), "a (#tag) stays");
    }

    #[test]
    fn a_template_still_to_be_written_says_nothing() {
        let notes = "What changed since 0.2.2:\n<!-- TODO: write the changes\n- not one\n-->\n";
        assert!(changes(notes).is_empty());
        assert!(changes("No list here.").is_empty());
    }

    #[test]
    fn bold_and_code_are_set_apart() {
        assert_eq!(
            spans("run `chmod +x` then **open** it"),
            vec![
                (Style::Plain, "run "),
                (Style::Code, "chmod +x"),
                (Style::Plain, " then "),
                (Style::Bold, "open"),
                (Style::Plain, " it"),
            ]
        );
        assert_eq!(
            spans("a lone ** stays"),
            vec![(Style::Plain, "a lone ** stays")]
        );
    }

    #[test]
    fn every_built_in_release_has_changes_and_comes_newest_first() {
        let releases = releases();
        assert!(!releases.is_empty());
        for pair in releases.windows(2) {
            assert!(crate::update::newer(pair[0].version, pair[1].version));
        }
        for release in &releases {
            assert!(!release.changes.is_empty(), "{}", release.version);
        }
    }

    #[test]
    fn whats_new_opens_after_an_update_only() {
        let current = env!("CARGO_PKG_VERSION");
        assert!(show_after_update(None), "a settings file from before");
        assert!(show_after_update(Some("0.0.1")));
        assert!(!show_after_update(Some(current)));
        assert!(!show_after_update(Some("999.0.0")), "a downgrade");
    }
}
