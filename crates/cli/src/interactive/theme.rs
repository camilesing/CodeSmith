//! The line REPL's accent color.
//!
//! Themes are a fullscreen concept: there are no panels, borders, or
//! backgrounds here to recolor. What a theme can still choose is the prompt
//! accent — and `/color` overrides it directly, which is the knob Claude
//! Code's `/color` turns. The status line's own segments keep their
//! semantic colors (cyan working, yellow waiting, magenta compacting), so
//! the dot does not follow the theme.
//!
//! Known limitations (recorded beside the behaviour it owns):
//!
//! - Only the prompt is themed. Segment labels, the state dot, dimmed hints,
//!   and warnings keep their semantic colors (`dimmed`, `yellow`, `red`),
//!   because those carry meaning rather than style.
//! - The names accepted here are the ones the repo's settings documentation
//!   already lists ([`THEME_NAMES`]); an unknown theme falls back to the
//!   terminal's own palette rather than failing a session over a color.

use colored::Color;

/// Theme names accepted by `/theme` and `theme = "..."`.
pub const THEME_NAMES: &[&str] = &[
    "terminal",
    "system",
    "dark",
    "light",
    "grayscale",
    "catppuccin-mocha",
    "tokyo-night",
    "dracula",
    "gruvbox-dark",
];

/// Color names accepted by `/color`: Claude Code's list plus the basic
/// ANSI names (`magenta`, `white`, `black`) that [`parse_color`] accepts
/// and every terminal can render.
pub const COLOR_NAMES: &[&str] = &[
    "red", "blue", "green", "yellow", "purple", "magenta", "orange", "pink", "cyan", "white",
    "black",
];

/// Parse a color name into a terminal color.
#[must_use]
pub fn parse_color(name: &str) -> Option<Color> {
    match name.trim().to_ascii_lowercase().as_str() {
        "red" => Some(Color::Red),
        "blue" => Some(Color::Blue),
        "green" => Some(Color::Green),
        "yellow" => Some(Color::Yellow),
        "purple" | "magenta" => Some(Color::Magenta),
        "orange" => Some(Color::TrueColor {
            r: 255,
            g: 165,
            b: 0,
        }),
        "pink" => Some(Color::TrueColor {
            r: 255,
            g: 192,
            b: 203,
        }),
        "cyan" => Some(Color::Cyan),
        "white" => Some(Color::White),
        "black" => Some(Color::Black),
        _ => None,
    }
}

/// The accent a theme (and an optional `/color` override) selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Accent {
    /// The prompt marker.
    pub prompt: Color,
}

impl Default for Accent {
    fn default() -> Self {
        Self {
            prompt: Color::Cyan,
        }
    }
}

/// Resolve the accent for a theme name and an optional prompt-color
/// override.
#[must_use]
pub fn accent(theme: &str, prompt_color: Option<&str>) -> Accent {
    let themed = match theme.trim().to_ascii_lowercase().as_str() {
        // "terminal" (the default) and "system" both mean "the terminal's
        // palette decides": keep the REPL's own cyan/green accents.
        "" | "terminal" | "system" | "dark" => Accent::default(),
        "light" => Accent {
            prompt: Color::Blue,
        },
        "grayscale" => Accent {
            prompt: Color::White,
        },
        "catppuccin-mocha" => Accent {
            prompt: Color::TrueColor {
                r: 203,
                g: 166,
                b: 247,
            },
        },
        "tokyo-night" => Accent {
            prompt: Color::TrueColor {
                r: 122,
                g: 162,
                b: 247,
            },
        },
        "dracula" => Accent {
            prompt: Color::TrueColor {
                r: 189,
                g: 147,
                b: 249,
            },
        },
        "gruvbox-dark" => Accent {
            prompt: Color::TrueColor {
                r: 215,
                g: 153,
                b: 33,
            },
        },
        _ => Accent::default(),
    };
    match prompt_color.and_then(parse_color) {
        Some(prompt) => Accent { prompt },
        None => themed,
    }
}

/// Whether `name` is a documented theme, so settings loading can warn on
/// typos instead of silently falling back.
#[must_use]
pub fn is_known_theme(name: &str) -> bool {
    let key = name.trim().to_ascii_lowercase();
    key.is_empty() || THEME_NAMES.contains(&key.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_theme_resolves_to_its_pinned_accent() {
        let pinned = [
            ("terminal", Color::Cyan),
            ("system", Color::Cyan),
            ("dark", Color::Cyan),
            ("light", Color::Blue),
            ("grayscale", Color::White),
            (
                "catppuccin-mocha",
                Color::TrueColor {
                    r: 203,
                    g: 166,
                    b: 247,
                },
            ),
            (
                "tokyo-night",
                Color::TrueColor {
                    r: 122,
                    g: 162,
                    b: 247,
                },
            ),
            (
                "dracula",
                Color::TrueColor {
                    r: 189,
                    g: 147,
                    b: 249,
                },
            ),
            (
                "gruvbox-dark",
                Color::TrueColor {
                    r: 215,
                    g: 153,
                    b: 33,
                },
            ),
        ];
        for (name, expected) in pinned {
            assert_eq!(accent(name, None).prompt, expected, "{name} drifted");
        }
        for name in THEME_NAMES {
            assert!(
                pinned.iter().any(|(n, _)| n == name),
                "{name} has no pinned accent"
            );
        }
        assert_eq!(accent("nonsense", None).prompt, Color::Cyan);
        assert_eq!(accent("", None).prompt, Color::Cyan);
    }

    #[test]
    fn advertised_color_names_and_parsed_color_names_match() {
        for name in COLOR_NAMES {
            assert!(
                parse_color(name).is_some(),
                "{name} advertised but unparsable"
            );
        }
        for name in [
            "red", "blue", "green", "yellow", "purple", "magenta", "orange", "pink", "cyan",
            "white", "black",
        ] {
            assert!(
                COLOR_NAMES.contains(&name),
                "{name} parses but /color never advertises it"
            );
        }
    }

    #[test]
    fn theme_recognition_covers_the_documented_names() {
        for name in THEME_NAMES {
            assert!(is_known_theme(name), "{name} must be known");
        }
        assert!(is_known_theme(""));
        assert!(is_known_theme(" Dracula "));
        assert!(!is_known_theme("dracul"));
    }

    #[test]
    fn color_overrides_the_theme_accent() {
        let themed = accent("dracula", None);
        assert_eq!(accent("dracula", Some("red")).prompt, Color::Red);
        assert_eq!(
            accent("dracula", Some("nope")).prompt,
            themed.prompt,
            "an unknown color name leaves the theme alone"
        );
        for name in COLOR_NAMES {
            assert!(parse_color(name).is_some(), "{name} must parse");
        }
    }
}
