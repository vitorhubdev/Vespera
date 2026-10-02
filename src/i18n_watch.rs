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
        "menu_item(ui",
        "menu_note(ui",
        "icon_button(ui",
        "Window::new(\"",
    ];

    fn literals(file: &str) -> Vec<(String, String)> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/ui")
            .join(file);
        let source = std::fs::read_to_string(&path).expect("source file");
        let lines: Vec<&str> = source.lines().collect();
        let mut found = Vec::new();
        // A widget call often spans lines after rustfmt: the literal may sit
        // up to two lines below the call. Join a 3-line window so those
        // literals are caught too.
        for index in 0..lines.len() {
            let window = [
                lines[index],
                lines.get(index + 1).copied().unwrap_or(""),
                lines.get(index + 2).copied().unwrap_or(""),
            ]
            .join(" ");
            // A comment or a doc line is not on screen.
            let trimmed = lines[index].trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if !CALLS.iter().any(|call| window.contains(call)) {
                continue;
            }
            let Some((_, tail)) = window.split_once('"') else {
                continue;
            };
            let Some(end) = tail.find('"') else {
                continue;
            };
            let literal = &tail[..end];
            // A literal that starts lower-case is a key, a name or a format.
            // A literal of only digits is a sample phone number.
            // The product name stays as it is in every language.
            // `env!` constants (CARGO_PKG_*) are build metadata, not UI text.
            let visible = !literal.is_empty()
                && literal != "Vespera"
                && !literal.starts_with("CARGO_PKG_")
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
