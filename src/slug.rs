/// Lowercase ASCII slug: [a-z0-9] plus `sep`-separated runs, max `max` chars.
/// Non-ASCII is transliterated first (å -> a, ö -> o, emoji -> words).
pub fn slugify(input: &str, max: usize) -> String {
    let ascii = deunicode::deunicode(input).to_lowercase();
    let mut out = String::with_capacity(ascii.len());
    let mut dash = false;
    for c in ascii.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if c == '_' && !out.is_empty() {
            out.push('_');
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let mut out: String = out.trim_matches(|c| c == '-' || c == '_').to_string();
    if out.len() > max {
        out.truncate(max);
        out = out.trim_end_matches(|c| c == '-' || c == '_').to_string();
    }
    out
}

/// Shortcode from an uploaded filename: stem without extension, slugified.
pub fn shortcode_from_filename(filename: &str) -> String {
    let stem = match filename.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => filename,
    };
    let s = slugify(stem, 48);
    if s.is_empty() { "sticker".into() } else { s }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Svenskt & Ölbröd!", 64), "svenskt-olbrod");
        assert_eq!(slugify("  --Memes--  ", 64), "memes");
        assert_eq!(slugify("surprised_pikachu", 64), "surprised_pikachu");
        assert_eq!(shortcode_from_filename("Surprised Pikachu.gif"), "surprised-pikachu");
        assert_eq!(shortcode_from_filename(".gif"), "gif");
        assert_eq!(shortcode_from_filename("???.webp"), "sticker");
        assert_eq!(slugify("abcdefghij-klm", 11), "abcdefghij");
    }
}
