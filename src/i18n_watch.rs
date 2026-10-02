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
        "soft_button(ui",
        "soft_button(ui, palette",
        "pill_button(ui, &palette, \"",
        "link(ui, \"",
        "search_field(",
        "theme::text(ui, \"",
        "hint_text(\"",
        "Button::new(\"",
        "Label::new(\"",
        "selectable_label(",
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
            // The product name stays as it is in every language.
            let visible = !literal.is_empty()
                && literal != "Vespera"
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
            for (_, literal) in literals(file) {
                leftover.push(format!("src/ui/{file}: {literal}"));
            }
        }
        leftover.sort();
        leftover.dedup();
        assert!(
            leftover.is_empty(),
            "these strings would stay English in every language; move them into \
             locales/*.json and use i18n::t:\n{}",
            leftover.join("\n")
        );
    }
}
