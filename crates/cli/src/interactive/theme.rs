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

/// Color names accepted by `/color`, matching Claude Code's list.
pub const COLOR_NAMES: &[&str] = &[
    "red", "blue", "green", "yellow", "purple", "orange", "pink", "cyan",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_documented_theme_resolves() {
        for name in THEME_NAMES {
            let accent = accent(name, None);
            // The fallback is the default accent, which is a legitimate
            // answer; what must not happen is a panic or a stray color for a
            // name that is documented.
            assert!(matches!(
                accent.prompt,
                Color::Cyan | Color::Blue | Color::White | Color::TrueColor { .. }
            ));
        }
        assert_eq!(accent("nonsense", None).prompt, Color::Cyan);
        assert_eq!(accent("", None).prompt, Color::Cyan);
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
