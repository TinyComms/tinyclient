//! Slack's names for the standard emoji people actually type. Rich text carries the code points
//! itself (`unicode`), so this table only serves mrkdwn, reactions and status emoji; a name that
//! is not here stays `:name:`, which is also what Slack shows for one it does not know.

const TABLE: &[(&str, &str)] = &[
    ("+1", "👍"), ("thumbsup", "👍"), ("-1", "👎"), ("thumbsdown", "👎"), ("100", "💯"),
    ("ok_hand", "👌"), ("clap", "👏"), ("wave", "👋"), ("pray", "🙏"), ("raised_hands", "🙌"),
    ("muscle", "💪"), ("point_up", "☝️"), ("point_down", "👇"), ("point_left", "👈"),
    ("point_right", "👉"), ("point_up_2", "👆"), ("v", "✌️"), ("crossed_fingers", "🤞"),
    ("handshake", "🤝"), ("writing_hand", "✍️"), ("eyes", "👀"), ("brain", "🧠"),
    ("smile", "😄"), ("smiley", "😃"), ("grin", "😁"), ("grinning", "😀"), ("joy", "😂"),
    ("rolling_on_the_floor_laughing", "🤣"), ("rofl", "🤣"), ("sweat_smile", "😅"),
    ("laughing", "😆"), ("satisfied", "😆"), ("wink", "😉"), ("blush", "😊"),
    ("slightly_smiling_face", "🙂"), ("upside_down_face", "🙃"), ("heart_eyes", "😍"),
    ("star-struck", "🤩"), ("kissing_heart", "😘"), ("yum", "😋"), ("stuck_out_tongue", "😛"),
    ("stuck_out_tongue_winking_eye", "😜"), ("sunglasses", "😎"), ("nerd_face", "🤓"),
    ("thinking_face", "🤔"), ("face_with_raised_eyebrow", "🤨"), ("neutral_face", "😐"),
    ("expressionless", "😑"), ("no_mouth", "😶"), ("smirk", "😏"), ("unamused", "😒"),
    ("face_with_rolling_eyes", "🙄"), ("grimacing", "😬"), ("relieved", "😌"),
    ("pensive", "😔"), ("sleepy", "😪"), ("sleeping", "😴"), ("mask", "😷"),
    ("face_with_thermometer", "🤒"), ("nauseated_face", "🤢"), ("sneezing_face", "🤧"),
    ("hot_face", "🥵"), ("cold_face", "🥶"), ("exploding_head", "🤯"),
    ("partying_face", "🥳"), ("confused", "😕"), ("worried", "😟"),
    ("slightly_frowning_face", "🙁"), ("open_mouth", "😮"), ("hushed", "😯"),
    ("astonished", "😲"), ("flushed", "😳"), ("pleading_face", "🥺"), ("cry", "😢"),
    ("sob", "😭"), ("scream", "😱"), ("disappointed", "😞"), ("sweat", "😓"), ("weary", "😩"),
    ("tired_face", "😫"), ("yawning_face", "🥱"), ("triumph", "😤"), ("rage", "😡"),
    ("angry", "😠"), ("skull", "💀"), ("poop", "💩"), ("hankey", "💩"), ("clown_face", "🤡"),
    ("ghost", "👻"), ("robot_face", "🤖"), ("see_no_evil", "🙈"), ("hear_no_evil", "🙉"),
    ("speak_no_evil", "🙊"), ("melting_face", "🫠"), ("saluting_face", "🫡"),
    ("face_holding_back_tears", "🥹"), ("hugging_face", "🤗"), ("shushing_face", "🤫"),
    ("zany_face", "🤪"), ("innocent", "😇"), ("smiling_imp", "😈"), ("facepalm", "🤦"),
    ("face_palm", "🤦"), ("shrug", "🤷"), ("man-shrugging", "🤷‍♂️"), ("woman-shrugging", "🤷‍♀️"),
    ("heart", "❤️"), ("orange_heart", "🧡"), ("yellow_heart", "💛"), ("green_heart", "💚"),
    ("blue_heart", "💙"), ("purple_heart", "💜"), ("black_heart", "🖤"), ("white_heart", "🤍"),
    ("broken_heart", "💔"), ("sparkling_heart", "💖"), ("two_hearts", "💕"),
    ("heavy_plus_sign", "➕"), ("heavy_minus_sign", "➖"), ("heavy_check_mark", "✔️"),
    ("white_check_mark", "✅"), ("ballot_box_with_check", "☑️"), ("x", "❌"),
    ("negative_squared_cross_mark", "❎"), ("question", "❓"), ("grey_question", "❔"),
    ("exclamation", "❗"), ("heavy_exclamation_mark", "❗"), ("bangbang", "‼️"),
    ("interrobang", "⁉️"), ("warning", "⚠️"), ("no_entry", "⛔"), ("no_entry_sign", "🚫"),
    ("rotating_light", "🚨"), ("sos", "🆘"), ("new", "🆕"), ("information_source", "ℹ️"),
    ("red_circle", "🔴"), ("large_orange_circle", "🟠"), ("large_yellow_circle", "🟡"),
    ("large_green_circle", "🟢"), ("large_blue_circle", "🔵"), ("white_circle", "⚪"),
    ("black_circle", "⚫"), ("large_green_square", "🟩"), ("large_red_square", "🟥"),
    ("fire", "🔥"), ("sparkles", "✨"), ("star", "⭐"), ("star2", "🌟"), ("zap", "⚡"),
    ("boom", "💥"), ("collision", "💥"), ("tada", "🎉"), ("confetti_ball", "🎊"),
    ("balloon", "🎈"), ("gift", "🎁"), ("trophy", "🏆"), ("medal", "🏅"),
    ("first_place_medal", "🥇"), ("rocket", "🚀"), ("airplane", "✈️"), ("car", "🚗"),
    ("ship", "🚢"), ("bulb", "💡"), ("mag", "🔍"), ("lock", "🔒"), ("unlock", "🔓"),
    ("key", "🔑"), ("bell", "🔔"), ("no_bell", "🔕"), ("mega", "📣"), ("loudspeaker", "📢"),
    ("speech_balloon", "💬"), ("thought_balloon", "💭"), ("zzz", "💤"), ("pushpin", "📌"),
    ("round_pushpin", "📍"), ("paperclip", "📎"), ("link", "🔗"), ("memo", "📝"),
    ("pencil", "📝"), ("pencil2", "✏️"), ("book", "📖"), ("books", "📚"), ("bookmark", "🔖"),
    ("clipboard", "📋"), ("calendar", "📆"), ("date", "📅"), ("spiral_calendar_pad", "🗓️"),
    ("chart_with_upwards_trend", "📈"), ("chart_with_downwards_trend", "📉"),
    ("bar_chart", "📊"), ("email", "✉️"), ("envelope", "✉️"), ("inbox_tray", "📥"),
    ("outbox_tray", "📤"), ("package", "📦"), ("file_folder", "📁"), ("open_file_folder", "📂"),
    ("wastebasket", "🗑️"), ("computer", "💻"), ("desktop_computer", "🖥️"), ("keyboard", "⌨️"),
    ("iphone", "📱"), ("phone", "☎️"), ("telephone_receiver", "📞"), ("headphones", "🎧"),
    ("microphone", "🎤"), ("camera", "📷"), ("video_camera", "📹"), ("tv", "📺"),
    ("hammer", "🔨"), ("wrench", "🔧"), ("hammer_and_wrench", "🛠️"), ("gear", "⚙️"),
    ("construction", "🚧"), ("bug", "🐛"), ("ant", "🐜"), ("beetle", "🐞"), ("snake", "🐍"),
    ("turtle", "🐢"), ("dog", "🐶"), ("cat", "🐱"), ("unicorn_face", "🦄"), ("crab", "🦀"),
    ("whale", "🐳"), ("octopus", "🐙"), ("penguin", "🐧"), ("bird", "🐦"), ("duck", "🦆"),
    ("owl", "🦉"), ("herb", "🌿"), ("seedling", "🌱"), ("evergreen_tree", "🌲"),
    ("palm_tree", "🌴"), ("cactus", "🌵"), ("four_leaf_clover", "🍀"), ("rose", "🌹"),
    ("sunflower", "🌻"), ("sunny", "☀️"), ("cloud", "☁️"), ("rainbow", "🌈"),
    ("umbrella", "☔"), ("snowflake", "❄️"), ("ocean", "🌊"), ("earth_americas", "🌎"),
    ("earth_africa", "🌍"), ("earth_asia", "🌏"), ("globe_with_meridians", "🌐"),
    ("moon", "🌙"), ("crescent_moon", "🌙"), ("coffee", "☕"), ("tea", "🍵"), ("beer", "🍺"),
    ("beers", "🍻"), ("wine_glass", "🍷"), ("cocktail", "🍸"), ("champagne", "🍾"),
    ("pizza", "🍕"), ("hamburger", "🍔"), ("fries", "🍟"), ("taco", "🌮"), ("burrito", "🌯"),
    ("sushi", "🍣"), ("ramen", "🍜"), ("cake", "🍰"), ("birthday", "🎂"), ("cookie", "🍪"),
    ("doughnut", "🍩"), ("apple", "🍎"), ("banana", "🍌"), ("lemon", "🍋"), ("avocado", "🥑"),
    ("popcorn", "🍿"), ("knife_fork_plate", "🍽️"), ("fork_and_knife", "🍴"),
    ("house", "🏠"), ("house_with_garden", "🏡"), ("office", "🏢"), ("hospital", "🏥"),
    ("school", "🏫"), ("bus", "🚌"), ("train", "🚆"), ("bike", "🚲"), ("runner", "🏃"),
    ("walking", "🚶"), ("dancer", "💃"), ("man_dancing", "🕺"), ("money_with_wings", "💸"),
    ("moneybag", "💰"), ("dollar", "💵"), ("credit_card", "💳"), ("gem", "💎"),
    ("crown", "👑"), ("dart", "🎯"), ("game_die", "🎲"), ("video_game", "🎮"),
    ("musical_note", "🎵"), ("notes", "🎶"), ("guitar", "🎸"), ("art", "🎨"),
    ("clapper", "🎬"), ("soccer", "⚽"), ("basketball", "🏀"), ("football", "🏈"),
    ("baseball", "⚾"), ("tennis", "🎾"), ("hourglass", "⌛"),
    ("hourglass_flowing_sand", "⏳"), ("alarm_clock", "⏰"), ("stopwatch", "⏱️"),
    ("clock1", "🕐"), ("watch", "⌚"), ("arrow_up", "⬆️"), ("arrow_down", "⬇️"),
    ("arrow_left", "⬅️"), ("arrow_right", "➡️"), ("arrows_counterclockwise", "🔄"),
    ("repeat", "🔁"), ("arrow_forward", "▶️"), ("double_vertical_bar", "⏸️"),
    ("black_square_for_stop", "⏹️"), ("recycle", "♻️"), ("infinity", "♾️"), ("tm", "™️"),
    ("copyright", "©️"), ("registered", "®️"), ("one", "1️⃣"), ("two", "2️⃣"), ("three", "3️⃣"),
    ("four", "4️⃣"), ("five", "5️⃣"), ("six", "6️⃣"), ("seven", "7️⃣"), ("eight", "8️⃣"),
    ("nine", "9️⃣"), ("zero", "0️⃣"), ("keycap_ten", "🔟"), ("thread", "🧵"), ("ship_it", "🚢"),
    ("spiral_note_pad", "🗒️"), ("test_tube", "🧪"), ("microscope", "🔬"), ("pill", "💊"),
    ("syringe", "💉"), ("thermometer", "🌡️"), ("shield", "🛡️"), ("crossed_swords", "⚔️"),
    ("bomb", "💣"), ("magic_wand", "🪄"), ("crystal_ball", "🔮"), ("mailbox", "📫"),
    ("battery", "🔋"), ("electric_plug", "🔌"), ("satellite", "🛰️"), ("flag-us", "🇺🇸"),
    ("checkered_flag", "🏁"), ("triangular_flag_on_post", "🚩"), ("white_flag", "🏳️"),
    ("rainbow-flag", "🏳️‍🌈"), ("baby", "👶"), ("man", "👨"), ("woman", "👩"),
    ("family", "👪"), ("bust_in_silhouette", "👤"), ("busts_in_silhouette", "👥"),
    ("technologist", "🧑‍💻"), ("male-technologist", "👨‍💻"), ("female-technologist", "👩‍💻"),
    ("spiral_calendar", "🗓️"), ("face_with_monocle", "🧐"), ("raised_hand", "✋"),
    ("hand", "✋"), ("call_me_hand", "🤙"), ("fist", "✊"), ("facepunch", "👊"),
    ("punch", "👊"), ("fist_bump", "👊"), ("heart_hands", "🫶"), ("salute", "🫡"),
    ("slack", "💬"), ("simple_smile", "🙂"), ("white_frowning_face", "☹️"),
    ("palm_tree_island", "🏝️"), ("desert_island", "🏝️"), ("beach_with_umbrella", "🏖️"),
    ("face_with_spiral_eyes", "😵‍💫"), ("dizzy_face", "😵"), ("dizzy", "💫"),
    ("sweat_drops", "💦"), ("dash", "💨"), ("anger", "💢"), ("heavy_dollar_sign", "💲"),
    ("heavy_multiplication_x", "✖️"), ("heavy_division_sign", "➗"), ("wavy_dash", "〰️"),
    ("small_blue_diamond", "🔹"), ("small_orange_diamond", "🔸"),
    ("large_blue_diamond", "🔷"), ("large_orange_diamond", "🔶"),
    ("white_small_square", "▫️"), ("black_small_square", "▪️"), ("pager", "📟"),
    ("spiral_bound_notebook", "📓"), ("notebook", "📓"), ("page_facing_up", "📄"),
    ("scroll", "📜"), ("newspaper", "📰"), ("label", "🏷️"), ("ticket", "🎫"),
    ("admission_tickets", "🎟️"), ("ladder", "🪜"), ("broom", "🧹"), ("soap", "🧼"),
    ("door", "🚪"), ("bed", "🛏️"), ("couch_and_lamp", "🛋️"), ("toilet", "🚽"),
    ("shower", "🚿"), ("world_map", "🗺️"), ("compass", "🧭"), ("mountain", "⛰️"),
    ("tent", "⛺"), ("camping", "🏕️"), ("fuelpump", "⛽"), ("anchor", "⚓"),
    ("christmas_tree", "🎄"), ("jack_o_lantern", "🎃"), ("fireworks", "🎆"),
    ("sparkler", "🎇"), ("snowman", "⛄"), ("santa", "🎅"),
];

/// The emoji for a Slack name, skin tone included: `+1::skin-tone-3`.
pub fn standard(name: &str) -> Option<String> {
    let (base, tone) = match name.split_once("::") {
        Some((base, tone)) => (base, Some(tone)),
        None => (name, None),
    };
    let (_, emoji) = TABLE.iter().find(|(known, _)| *known == base)?;
    let mut emoji = (*emoji).to_owned();
    if let Some(tone) = tone.and_then(skin_tone) {
        // The modifier follows the base character, before any variation selector.
        let at = emoji.chars().next().map_or(0, char::len_utf8);
        let rest = emoji.split_off(at);
        emoji.push(tone);
        emoji.push_str(rest.trim_start_matches('\u{fe0f}'));
    }
    Some(emoji)
}

/// Slack's name for an emoji, the other way round, for a reaction the user picked: `👍🏼` is
/// `+1::skin-tone-3`. `None` for one the table does not hold. Where Slack has two names for
/// one emoji the first listed is used; they are the same reaction to Slack.
pub fn name(emoji: &str) -> Option<String> {
    let plain = |text: &str| text.replace('\u{fe0f}', "");
    let find = |emoji: &str| TABLE.iter().find(|(_, known)| plain(known) == emoji).map(|(name, _)| *name);
    let wanted = plain(emoji);
    if let Some(name) = find(&wanted) {
        return Some(name.to_owned());
    }
    let tone = wanted.chars().find(|c| ('\u{1f3fb}'..='\u{1f3ff}').contains(c))?;
    let base: String = wanted.chars().filter(|c| *c != tone).collect();
    let level = tone as u32 - 0x1f3fb + 2;
    find(&base).map(|name| format!("{name}::skin-tone-{level}"))
}

/// `skin-tone-2` to `skin-tone-6` are the five Fitzpatrick modifiers.
pub fn skin_tone(name: &str) -> Option<char> {
    let level: u32 = name.strip_prefix("skin-tone-")?.parse().ok()?;
    (2..=6).contains(&level).then(|| char::from_u32(0x1f3fb + level - 2)).flatten()
}

/// Rich text names the code points itself: `1f44d-1f3fd`.
pub fn from_code_points(unicode: &str) -> Option<String> {
    let points = unicode.split('-').map(|hex| u32::from_str_radix(hex, 16).ok().and_then(char::from_u32));
    let emoji: Option<String> = points.collect();
    emoji.filter(|emoji| !emoji.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_tones_and_code_points_become_the_emoji_itself() {
        assert_eq!(standard("tada").as_deref(), Some("🎉"));
        assert_eq!(standard("+1::skin-tone-3").as_deref(), Some("👍🏼"));
        assert_eq!(standard("partyparrot"), None);
        assert_eq!(from_code_points("1f44d-1f3fd").as_deref(), Some("👍🏽"));
        assert_eq!(from_code_points("zz"), None);
    }

    #[test]
    fn an_emoji_the_user_picked_gets_the_name_slack_reacts_with() {
        assert_eq!(name("👀").as_deref(), Some("eyes"));
        assert_eq!(name("👍").as_deref(), Some("+1"));
        assert_eq!(name("👍🏼").as_deref(), Some("+1::skin-tone-3"));
        // With or without the variation selector a keyboard may add.
        assert_eq!(name("❤").as_deref(), Some("heart"));
        assert_eq!(name("❤️").as_deref(), Some("heart"));
        assert_eq!(name("🫥"), None);
        for (known, emoji) in TABLE {
            let back = name(emoji).unwrap();
            assert_eq!(standard(&back).as_deref(), Some(*emoji), "{known}");
        }
    }

    #[test]
    fn no_name_is_listed_twice() {
        for (at, (name, _)) in TABLE.iter().enumerate() {
            assert!(!TABLE[..at].iter().any(|(earlier, _)| earlier == name), "{name}");
        }
    }
}
