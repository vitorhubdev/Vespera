//! Watches the UI source for text that would never reach the catalog.
//!
//! The translation round moves the visible strings into `locales/*.json`. A
//! literal dropped into a widget call stays English forever and nobody notices
//! until a user in Portuguese sees it, so this test reads the sources and
//! fails on anything new.

#[cfg(test)]
mod tests {
    /// Files read by the test, in a fixed order.
    const FILES: &[&str] = &[
        "calls.rs",
        "chats.rs",
        "conversation.rs",
        "dialogs.rs",
        "favorites.rs",
        "login.rs",
        "mod.rs",
        "picker.rs",
        "polls.rs",
        "settings.rs",
        "status.rs",
        "update.rs",
        "viewer.rs",
        "widgets.rs",
    ];

    /// The calls that put a literal in front of the user.
    const CALLS: &[&str] = &[
        "label(\"",
        "on_hover_text(\"",
        "RichText::new(\"",
        "heading(\"",
        "soft_button(ui, &palette, None, \"",
        "pill_button(ui, &palette, \"",
        "link(ui, \"",
        "search_field(",
    ];

    /// Text still in the source, listed by file and by the literal itself.
    /// Each entry is a line the sweep still has to move; a new literal outside
    /// this list fails the test instead of shipping in English. Line numbers
    /// are left out on purpose, so moving code around does not churn it.
    const KNOWN: &[(&str, &str)] = &[
        ("conversation.rs", "All shortcuts ({})"),
        ("conversation.rs", "Delete"),
        ("conversation.rs", "Forward"),
        ("conversation.rs", "Forwarded"),
        ("conversation.rs", "Open in a map"),
        ("conversation.rs", "Playback speed"),
        ("conversation.rs", "Remove your reaction"),
        ("dialogs.rs", "Add to my packs"),
        ("dialogs.rs", "Add"),
        ("dialogs.rs", "Approve"),
        ("dialogs.rs", "Cancel"),
        ("dialogs.rs", "Check for updates"),
        ("dialogs.rs", "Choose a folder"),
        ("dialogs.rs", "Choose photo…"),
        ("dialogs.rs", "Close"),
        ("dialogs.rs", "Confirm"),
        ("dialogs.rs", "Copy link"),
        ("dialogs.rs", "Create"),
        ("dialogs.rs", "Deny"),
        ("dialogs.rs", "Export chat"),
        ("dialogs.rs", "Forward"),
        ("dialogs.rs", "Get a code"),
        ("dialogs.rs", "Invite link"),
        ("dialogs.rs", "Join requests"),
        ("dialogs.rs", "Message"),
        ("dialogs.rs", "New group"),
        ("dialogs.rs", "Remove photo"),
        ("dialogs.rs", "Remove"),
        ("dialogs.rs", "Revoke link"),
        ("dialogs.rs", "Save contact"),
        ("dialogs.rs", "Save description"),
        ("dialogs.rs", "Save name"),
        ("dialogs.rs", "Save sticker"),
        ("dialogs.rs", "Send sticker"),
        ("dialogs.rs", "Source code"),
        ("mod.rs", "Cancel"),
        ("mod.rs", "Show the chat list (Ctrl+B)"),
        (
            "picker.rs",
            "Import a .wastickers, .zip, or folder of stickers",
        ),
        ("polls.rs", "Cancel"),
        ("viewer.rs", "Copy the picture to the clipboard"),
        ("viewer.rs", "Open in the default app"),
        ("viewer.rs", "Turn the page (R)"),
    ];

    fn literals(file: &str) -> Vec<(String, String)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/ui")
            .join(file);
        let source = std::fs::read_to_string(&path).expect("source file");
        let mut found = Vec::new();
        for line in source.lines() {
            // A comment or a doc line is not on screen.
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if !CALLS.iter().any(|call| line.contains(call)) {
                continue;
            }
            let Some((_, tail)) = line.split_once('"') else {
                continue;
            };
            let Some(end) = tail.find('"') else {
                continue;
            };
            let literal = &tail[..end];
            // A literal that starts lower-case is a key, a name or a format.
            // A literal of only digits is a sample phone number.
            let visible = !literal.is_empty()
                && literal.chars().any(|c| !c.is_ascii_digit())
                && literal
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_uppercase() || first == '{');
            if visible {
                found.push((file.to_owned(), literal.to_owned()));
            }
        }
        found
    }

    #[test]
    fn ui_text_goes_through_the_catalog() {
        let mut leftover = Vec::new();
        for file in FILES {
            let mut seen = std::collections::HashSet::new();
            for (file, literal) in literals(file) {
                if !seen.insert((file.clone(), literal.clone())) {
                    continue;
                }
                if !KNOWN
                    .iter()
                    .any(|(known_file, known)| known_file == &file && known == &literal)
                {
                    leftover.push(format!("src/ui/{file}: {literal}"));
                }
            }
        }
        assert!(
            leftover.is_empty(),
            "these strings would stay English in every language; move them into \
             locales/*.json and use i18n::t:\n{}",
            leftover.join("\n")
        );
    }

    #[test]
    fn the_known_list_is_still_true() {
        // A pending line that no longer exists was moved: drop it from the
        // list, otherwise the test goes quiet about real new text.
        let mut leftovers: Vec<String> = Vec::new();
        for file in FILES {
            for (known_file, literal) in KNOWN {
                if known_file == file
                    && !literals(file).contains(&(known_file.to_string(), literal.to_string()))
                {
                    leftovers.push(format!("src/ui/{known_file}: {literal}"));
                }
            }
        }
        leftovers.sort();
        leftovers.dedup();
        assert!(
            leftovers.is_empty(),
            "KNOWN lists text that is gone; remove those entries:\n{}",
            leftovers.join("\n")
        );
    }
}
