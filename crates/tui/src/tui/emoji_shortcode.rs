//! `:shortcode:` emoji completion for the composer.
//!
//! Two behaviours, mirroring Claude Code's `:` prefix input:
//!
//! 1. Typing the closing `:` of a known `:name:` token replaces the whole
//!    token with the emoji (see [`shortcode_closeable_at`]).
//! 2. Typing at least two characters of a partial `:na…` token raises the
//!    emoji popup (see [`partial_shortcode_at_cursor`] and
//!    [`visible_emoji_menu_entries`]).
//!
//! The opening `:` is only recognized at the start of the input or right
//! after whitespace, so URLs (`https://…`) and times (`12:30`) never
//! trigger completion. The table is a static, dependency-free list of
//! common shortcodes — unknown names stay plain text.

use super::app::App;

/// Maximum rows shown in the emoji popup.
pub(crate) const EMOJI_MENU_LIMIT: usize = 8;

/// Minimum typed characters (after the `:`) before popup suggestions appear.
const EMOJI_MENU_MIN_CHARS: usize = 2;

/// Static shortcode → emoji table, alphabetically grouped. Curated to the
/// shortcodes people actually type (GitHub / Claude Code conventions).
const EMOJI_SHORTCODES: &[(&str, &str)] = &[
    // smiles & faces
    ("smile", "😄"),
    ("smiley", "😃"),
    ("grin", "😁"),
    ("laughing", "😆"),
    ("joy", "😂"),
    ("rofl", "🤣"),
    ("wink", "😉"),
    ("blush", "😊"),
    ("innocent", "😇"),
    ("heart_eyes", "😍"),
    ("kissing_heart", "😘"),
    ("yum", "😋"),
    ("stuck_out_tongue_winking_eye", "😜"),
    ("zany_face", "🤪"),
    ("raised_eyebrow", "🤨"),
    ("neutral_face", "😐"),
    ("expressionless", "😑"),
    ("no_mouth", "😶"),
    ("smirk", "😏"),
    ("unamused", "😒"),
    ("roll_eyes", "🙄"),
    ("grimacing", "😬"),
    ("lying_face", "🤥"),
    ("relieved", "😌"),
    ("pensive", "😔"),
    ("sleepy", "😪"),
    ("drooling_face", "🤤"),
    ("sleeping", "😴"),
    ("mask", "😷"),
    ("face_with_thermometer", "🤒"),
    ("nauseated_face", "🤢"),
    ("sneezing_face", "🤧"),
    ("hot_face", "🥵"),
    ("cold_face", "🥶"),
    ("woozy_face", "🥴"),
    ("exploding_head", "🤯"),
    ("cowboy_hat_face", "🤠"),
    ("partying_face", "🥳"),
    ("disguised_face", "🥸"),
    ("sunglasses", "😎"),
    ("nerd_face", "🤓"),
    ("monocle_face", "🧐"),
    ("confused", "😕"),
    ("worried", "😟"),
    ("slightly_frowning_face", "🙁"),
    ("frowning_face", "☹️"),
    ("open_mouth", "😮"),
    ("hushed", "😯"),
    ("astonished", "😲"),
    ("flushed", "😳"),
    ("scream", "😱"),
    ("fearful", "😨"),
    ("cold_sweat", "😰"),
    ("tired_face", "😫"),
    ("weary", "😩"),
    ("pleading_face", "🥺"),
    ("cry", "😢"),
    ("sob", "😭"),
    ("angry", "😠"),
    ("rage", "😡"),
    ("cursing_face", "🤬"),
    ("skull", "💀"),
    ("ghost", "👻"),
    ("alien", "👽"),
    ("robot", "🤖"),
    ("poop", "💩"),
    ("clown_face", "🤡"),
    // gestures & body
    ("+1", "👍"),
    ("thumbsup", "👍"),
    ("-1", "👎"),
    ("thumbsdown", "👎"),
    ("ok_hand", "👌"),
    ("pinched_fingers", "🤌"),
    ("peace", "✌️"),
    ("crossed_fingers", "🤞"),
    ("vulcan_salute", "🖖"),
    ("metal", "🤘"),
    ("wave", "👋"),
    ("raised_hand", "✋"),
    ("open_hands", "🤲"),
    ("handshake", "🤝"),
    ("pray", "🙏"),
    ("muscle", "💪"),
    ("point_left", "👈"),
    ("point_right", "👉"),
    ("point_up_2", "👆"),
    ("point_down", "👇"),
    ("writing_hand", "✍️"),
    ("eyes", "👀"),
    ("eye", "👁️"),
    ("brain", "🧠"),
    ("ear", "👂"),
    ("nose", "👃"),
    ("lips", "👄"),
    ("tongue", "👅"),
    ("shrug", "🤷"),
    ("facepalm", "🤦"),
    ("bow", "🙇"),
    ("raising_hand", "🙋"),
    // hearts & symbols
    ("heart", "❤️"),
    ("broken_heart", "💔"),
    ("sparkling_heart", "💖"),
    ("two_hearts", "💕"),
    ("heartbeat", "💗"),
    ("heartpulse", "💓"),
    ("blue_heart", "💙"),
    ("green_heart", "💚"),
    ("yellow_heart", "💛"),
    ("purple_heart", "💜"),
    ("black_heart", "🖤"),
    ("white_heart", "🤍"),
    ("rainbow", "🌈"),
    ("fire", "🔥"),
    ("sparkles", "✨"),
    ("star", "⭐"),
    ("stars", "🌟"),
    ("dizzy", "💫"),
    ("boom", "💥"),
    ("zap", "⚡"),
    ("sunny", "☀️"),
    ("moon", "🌙"),
    ("crescent_moon", "🌙"),
    ("full_moon", "🌕"),
    ("cloud", "☁️"),
    ("rain", "🌧️"),
    ("snowflake", "❄️"),
    ("snowman", "⛄"),
    ("umbrella", "☂️"),
    ("ocean", "🌊"),
    // celebration & objects
    ("tada", "🎉"),
    ("confetti_ball", "🎊"),
    ("balloon", "🎈"),
    ("gift", "🎁"),
    ("birthday", "🎂"),
    ("cake", "🍰"),
    ("trophy", "🏆"),
    ("medal", "🏅"),
    ("crown", "👑"),
    ("gem", "💎"),
    ("ring", "💍"),
    ("money_with_wings", "💸"),
    ("moneybag", "💰"),
    ("dollar", "💵"),
    ("chart_with_upwards_trend", "📈"),
    ("chart_with_downwards_trend", "📉"),
    ("bar_chart", "📊"),
    ("clipboard", "📋"),
    ("memo", "📝"),
    ("notebook", "📓"),
    ("book", "📖"),
    ("books", "📚"),
    ("newspaper", "📰"),
    ("calendar", "📅"),
    ("pushpin", "📌"),
    ("paperclip", "📎"),
    ("scissors", "✂️"),
    ("pencil", "✏️"),
    ("lock", "🔒"),
    ("unlock", "🔓"),
    ("key", "🔑"),
    ("hammer", "🔨"),
    ("wrench", "🔧"),
    ("gear", "⚙️"),
    ("bulb", "💡"),
    ("battery", "🔋"),
    ("candle", "🕯️"),
    ("warning", "⚠️"),
    ("no_entry_sign", "🚫"),
    ("recycle", "♻️"),
    ("white_check_mark", "✅"),
    ("ballot_box_with_check", "☑️"),
    ("heavy_check_mark", "✔️"),
    ("x", "❌"),
    ("question", "❓"),
    ("exclamation", "❗"),
    ("hundred", "💯"),
    ("infinity", "♾️"),
    ("checkered_flag", "🏁"),
    ("triangular_flag_on_post", "🚩"),
    // arrows, media & time
    ("arrow_right", "➡️"),
    ("arrow_left", "⬅️"),
    ("arrow_up", "⬆️"),
    ("arrow_down", "⬇️"),
    ("arrows_counterclockwise", "🔄"),
    ("repeat", "🔁"),
    ("fast_forward", "⏩"),
    ("rewind", "⏪"),
    ("play", "▶️"),
    ("pause", "⏸️"),
    ("stop", "⏹️"),
    ("hourglass", "⌛"),
    ("watch", "⌚"),
    ("alarm_clock", "⏰"),
    // tech
    ("computer", "💻"),
    ("desktop_computer", "🖥️"),
    ("keyboard", "⌨️"),
    ("mouse", "🖱️"),
    ("printer", "🖨️"),
    ("floppy_disk", "💾"),
    ("phone", "📱"),
    ("telephone", "☎️"),
    ("tv", "📺"),
    ("camera", "📷"),
    ("video_camera", "📹"),
    ("microphone", "🎤"),
    ("headphones", "🎧"),
    ("radio", "📻"),
    ("speaker", "🔊"),
    ("bell", "🔔"),
    ("no_bell", "🔕"),
    ("mag", "🔍"),
    ("rocket", "🚀"),
    ("airplane", "✈️"),
    ("flight_departure", "🛫"),
    ("flight_arrival", "🛬"),
    ("helicopter", "🚁"),
    ("sailboat", "⛵"),
    ("anchor", "⚓"),
    ("construction", "🚧"),
    ("car", "🚗"),
    ("taxi", "🚕"),
    ("bus", "🚌"),
    ("train", "🚆"),
    ("metro", "🚇"),
    ("bike", "🚲"),
    ("motorcycle", "🏍️"),
    ("ship", "🚢"),
    // animals & nature
    ("dog", "🐕"),
    ("cat", "🐈"),
    ("mouse_animal", "🐭"),
    ("hamster", "🐹"),
    ("rabbit", "🐰"),
    ("fox_face", "🦊"),
    ("bear", "🐻"),
    ("panda_face", "🐼"),
    ("koala", "🐨"),
    ("tiger", "🐯"),
    ("lion", "🦁"),
    ("cow", "🐮"),
    ("pig", "🐷"),
    ("frog", "🐸"),
    ("monkey_face", "🐵"),
    ("see_no_evil", "🙈"),
    ("hear_no_evil", "🙉"),
    ("speak_no_evil", "🙊"),
    ("penguin", "🐧"),
    ("bird", "🐦"),
    ("baby_chick", "🐤"),
    ("eagle", "🦅"),
    ("duck", "🦆"),
    ("owl", "🦉"),
    ("wolf", "🐺"),
    ("horse", "🐴"),
    ("unicorn", "🦄"),
    ("bee", "🐝"),
    ("bug", "🐛"),
    ("butterfly", "🦋"),
    ("snail", "🐌"),
    ("beetle", "🐞"),
    ("spider", "🕷️"),
    ("turtle", "🐢"),
    ("snake", "🐍"),
    ("octopus", "🐙"),
    ("shrimp", "🦐"),
    ("crab", "🦀"),
    ("tropical_fish", "🐠"),
    ("fish", "🐟"),
    ("dolphin", "🐬"),
    ("shark", "🦈"),
    ("whale", "🐳"),
    ("elephant", "🐘"),
    ("dinosaur", "🦖"),
    ("dragon", "🐉"),
    ("cactus", "🌵"),
    ("evergreen_tree", "🌲"),
    ("deciduous_tree", "🌳"),
    ("palm_tree", "🌴"),
    ("seedling", "🌱"),
    ("herb", "🌿"),
    ("four_leaf_clover", "🍀"),
    ("maple_leaf", "🍁"),
    ("fallen_leaf", "🍂"),
    ("mushroom", "🍄"),
    ("blossom", "🌸"),
    ("rose", "🌹"),
    ("sunflower", "🌻"),
    ("tulip", "🌷"),
    ("bouquet", "💐"),
    // food & drink
    ("apple", "🍎"),
    ("orange", "🍊"),
    ("lemon", "🍋"),
    ("banana", "🍌"),
    ("watermelon", "🍉"),
    ("grapes", "🍇"),
    ("strawberry", "🍓"),
    ("cherries", "🍒"),
    ("peach", "🍑"),
    ("pineapple", "🍍"),
    ("mango", "🥭"),
    ("coconut", "🥥"),
    ("kiwi", "🥝"),
    ("tomato", "🍅"),
    ("avocado", "🥑"),
    ("broccoli", "🥦"),
    ("carrot", "🥕"),
    ("hot_pepper", "🌶️"),
    ("corn", "🌽"),
    ("potato", "🥔"),
    ("bread", "🍞"),
    ("croissant", "🥐"),
    ("cheese", "🧀"),
    ("egg", "🥚"),
    ("cooking", "🍳"),
    ("bacon", "🥓"),
    ("steak", "🥩"),
    ("hamburger", "🍔"),
    ("fries", "🍟"),
    ("pizza", "🍕"),
    ("hotdog", "🌭"),
    ("sandwich", "🥪"),
    ("taco", "🌮"),
    ("burrito", "🌯"),
    ("salad", "🥗"),
    ("ramen", "🍜"),
    ("spaghetti", "🍝"),
    ("sushi", "🍣"),
    ("curry", "🍛"),
    ("rice", "🍚"),
    ("icecream", "🍦"),
    ("doughnut", "🍩"),
    ("cookie", "🍪"),
    ("candy", "🍬"),
    ("lollipop", "🍭"),
    ("honey", "🍯"),
    ("milk", "🥛"),
    ("coffee", "☕"),
    ("tea", "🍵"),
    ("champagne", "🍾"),
    ("wine", "🍷"),
    ("cocktail", "🍸"),
    ("beer", "🍺"),
    ("beers", "🍻"),
    // places
    ("house", "🏠"),
    ("office", "🏢"),
    ("hospital", "🏥"),
    ("bank", "🏦"),
    ("hotel", "🏨"),
    ("school", "🏫"),
    ("factory", "🏭"),
    ("castle", "🏰"),
    ("church", "⛪"),
    ("fountain", "⛲"),
    ("tent", "⛺"),
    ("night_with_stars", "🌃"),
    ("sunrise", "🌅"),
    ("cityscape", "🏙️"),
    ("bridge_at_night", "🌉"),
    ("milky_way", "🌌"),
    ("ferris_wheel", "🎡"),
    ("roller_coaster", "🎢"),
    ("map", "🗺️"),
    ("mount_fuji", "🗻"),
    ("jack_o_lantern", "🎃"),
    ("fireworks", "🎆"),
    ("christmas_tree", "🎄"),
];

/// Look up a complete shortcode name.
pub(crate) fn lookup(name: &str) -> Option<&'static str> {
    EMOJI_SHORTCODES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, emoji)| *emoji)
}

/// Shortcodes matching a partial prefix, exact match first, capped at `limit`.
pub(crate) fn matching(prefix: &str, limit: usize) -> Vec<(&'static str, &'static str)> {
    let normalized = prefix.to_ascii_lowercase();
    let mut exact = Vec::new();
    let mut prefixed = Vec::new();
    for (name, emoji) in EMOJI_SHORTCODES {
        if *name == normalized {
            exact.push((*name, *emoji));
        } else if name.starts_with(&normalized) {
            prefixed.push((*name, *emoji));
        }
    }
    exact.append(&mut prefixed);
    exact.truncate(limit);
    exact
}

fn is_shortcode_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '+' || ch == '-'
}

/// The char before `pos` is a valid opening `:` when it starts the input or
/// follows whitespace.
fn is_colon_boundary(chars: &[char], colon_index: usize) -> bool {
    colon_index == 0 || chars[colon_index - 1].is_whitespace()
}

/// Find the partial `:name` token under the cursor. Returns the byte offset
/// of the opening `:` and the partial name typed so far (may be empty).
pub(crate) fn partial_shortcode_at_cursor(
    input: &str,
    cursor_chars: usize,
) -> Option<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    if cursor_chars > chars.len() {
        return None;
    }
    let mut index = cursor_chars;
    while index > 0 && is_shortcode_name_char(chars[index - 1]) {
        index -= 1;
    }
    if index == 0 || chars[index - 1] != ':' || !is_colon_boundary(&chars, index - 1) {
        return None;
    }
    let partial: String = chars[index..cursor_chars].iter().collect();
    // Byte offset of the ':' itself (index - 1), not of the first name char.
    let colon_byte: usize = chars[..index - 1].iter().map(|c| c.len_utf8()).sum();
    Some((colon_byte, partial))
}

/// When the text immediately before the cursor ends with a known `:name`
/// token (i.e. the `:` the user is about to type would close it), return
/// the byte offset of the opening `:` and the emoji to replace the token
/// with. Called before the closing colon is inserted.
pub(crate) fn shortcode_closeable_at(
    input: &str,
    cursor_chars: usize,
) -> Option<(usize, &'static str)> {
    let chars: Vec<char> = input.chars().collect();
    if cursor_chars == 0 || cursor_chars > chars.len() {
        return None;
    }
    let mut index = cursor_chars;
    while index > 0 && is_shortcode_name_char(chars[index - 1]) {
        index -= 1;
    }
    if index == 0 || chars[index - 1] != ':' || !is_colon_boundary(&chars, index - 1) {
        return None;
    }
    let name: String = chars[index..cursor_chars].iter().collect();
    if name.is_empty() {
        return None;
    }
    let emoji = lookup(&name)?;
    // Byte offset of the ':' itself (index - 1), not of the first name char.
    let colon_byte: usize = chars[..index - 1].iter().map(|c| c.len_utf8()).sum();
    Some((colon_byte, emoji))
}

/// Emoji popup entries for the current composer state. Empty unless a
/// partial `:token` with at least [`EMOJI_MENU_MIN_CHARS`] characters sits
/// at the cursor and the user hasn't dismissed the popup for this exact
/// (input, cursor) position.
pub(crate) fn visible_emoji_menu_entries(app: &App) -> Vec<(&'static str, &'static str)> {
    if app.is_history_search_active() {
        return Vec::new();
    }
    let Some((_, partial)) = partial_shortcode_at_cursor(&app.input, app.cursor_position) else {
        return Vec::new();
    };
    if partial.chars().count() < EMOJI_MENU_MIN_CHARS {
        return Vec::new();
    }
    if let Some((suppressed_input, suppressed_cursor)) = &app.emoji_menu_suppressed_at
        && *suppressed_input == app.input
        && *suppressed_cursor == app.cursor_position
    {
        return Vec::new();
    }
    matching(&partial, EMOJI_MENU_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_finds_known_shortcode() {
        assert_eq!(lookup("fire"), Some("🔥"));
        assert_eq!(lookup("thumbsup"), Some("👍"));
        assert_eq!(lookup("nope_unknown"), None);
    }

    #[test]
    fn matching_ranks_exact_match_first_and_caps_insensitively() {
        let entries = matching("Fire", 8);
        assert_eq!(entries.first().map(|(name, _)| *name), Some("fire"));
        let thumbs = matching("thumbs", 8);
        assert!(thumbs.iter().any(|(name, _)| *name == "thumbsup"));
    }

    #[test]
    fn partial_shortcode_requires_boundary_before_colon() {
        let (byte, partial) = partial_shortcode_at_cursor("hi :thumbsmi", 12).unwrap();
        assert_eq!(&"hi :thumbsmi"[byte..byte + 1], ":");
        assert_eq!(partial, "thumbsmi");

        // URL scheme colon — preceded by 's', not whitespace.
        assert!(partial_shortcode_at_cursor("see https://ex", 12).is_none());
        // Time — preceded by a digit.
        assert!(partial_shortcode_at_cursor("meet at 12:30", 12).is_none());
        // No colon at all.
        assert!(partial_shortcode_at_cursor("hello", 5).is_none());
    }

    #[test]
    fn closeable_shortcode_spans_known_names_only() {
        let input = "nice :fire";
        let (byte, emoji) = shortcode_closeable_at(input, input.chars().count()).unwrap();
        assert_eq!(&input[byte..], ":fire");
        assert_eq!(emoji, "🔥");

        // Unknown name stays text.
        let unknown = "nice :notanemoji";
        assert!(shortcode_closeable_at(unknown, unknown.chars().count()).is_none());
        // `12:30` — the name part is digits after a non-boundary colon.
        let time = "at 12:30";
        assert!(shortcode_closeable_at(time, time.chars().count()).is_none());
        // Empty name (`:` then the closing `:`) is not a shortcode.
        assert!(shortcode_closeable_at("hi :", 4).is_none());
    }

    #[test]
    fn table_has_no_duplicate_names() {
        let mut names: Vec<&str> = EMOJI_SHORTCODES.iter().map(|(name, _)| *name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "duplicate shortcode names in table");
    }
}
