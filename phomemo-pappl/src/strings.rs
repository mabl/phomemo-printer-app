//! The application's English strings catalog (`docs/overprint-plan.md`, D8).
//!
//! `c/main.c` registers it with `papplSystemAddStringsData` for `en`. PAPPL
//! merges it into its own `en` strings, which it loaded first, and the first
//! entry for a key wins (`loc.c`), so the catalog adds names but cannot
//! change PAPPL's. PAPPL's web interface looks names up in it
//! (`media.<size name>`, a vendor option's name, `<option>.<keyword>`), and
//! serves it at `printer-strings-uri` for IPP clients. PAPPL keeps a pointer
//! to the text rather than a copy, so it is built once and kept for the
//! process's lifetime.

use std::ffi::{CStr, CString, c_char};
use std::fmt::Write as _;
use std::ptr;
use std::sync::LazyLock;

use crate::models::Model;
use crate::overprint::VerticalPolicy;

/// The vendor option of the overprint vertical policy.
const OVERPRINT_VERTICAL: &str = "phomemo-overprint-vertical";

/// A strings catalog: its pairs, and the text of its `.strings` file.
#[derive(Debug)]
struct Catalog {
    pairs: Vec<(CString, CString)>,
    text: CString,
}

static EN: LazyLock<Catalog> = LazyLock::new(|| Catalog::new(&en_pairs()));

impl Catalog {
    /// The catalog of `pairs`, whose keys are distinct; a pair holding a
    /// NUL, which a C string cannot, is left out.
    fn new(pairs: &[(String, String)]) -> Self {
        let pairs: Vec<_> = pairs
            .iter()
            .filter_map(|(key, text)| {
                Some((
                    CString::new(key.as_str()).ok()?,
                    CString::new(text.as_str()).ok()?,
                ))
            })
            .collect();
        // No comments: PAPPL 1.4.12's reader (loc.c, loc_load_resource)
        // goes on to read a key straight after one and stops at a "missing
        // separator". Its loop also skips the character after each `;`
        // unread, so every pair ends with a newline: a key straight after
        // a `;` would lose its opening quote, and a `;` at the very end
        // would make it step over the terminating NUL and read past it.
        let mut text = String::new();
        for (key, value) in &pairs {
            let _ = writeln!(
                text,
                "\"{}\" = \"{}\";",
                escape(&key.to_string_lossy()),
                escape(&value.to_string_lossy())
            );
        }
        Self {
            pairs,
            // Every pair is free of NUL, and so is the rest.
            text: CString::new(text).unwrap_or_default(),
        }
    }

    /// The text for `key`, if the catalog has it.
    fn get(&self, key: &CStr) -> Option<&CStr> {
        self.pairs
            .iter()
            .find(|(candidate, _)| candidate.as_c_str() == key)
            .map(|(_, text)| text.as_c_str())
    }
}

/// The English pairs: every model's canvas names, then the vendor options'
/// names and keywords.
fn en_pairs() -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut add = |key: String, text: &str| {
        if !pairs.iter().any(|(existing, _)| *existing == key) {
            pairs.push((key, text.to_owned()));
        }
    };
    for model in Model::all() {
        for profile in model.overprint_profiles() {
            add(format!("media.{}", profile.canvas_name()), profile.label);
        }
    }
    add(OVERPRINT_VERTICAL.to_owned(), "Overprint (vertical)");
    for policy in VerticalPolicy::ALL {
        add(
            format!("{OVERPRINT_VERTICAL}.{}", policy.name()),
            policy.label(),
        );
    }
    add("phomemo-dither".to_owned(), "Dithering");
    add("phomemo-compression".to_owned(), "Compression");
    pairs
}

/// `text` escaped for a quoted string in a `.strings` file, as PAPPL's
/// `loc.c` reads one: `\` and `"` with a backslash, newlines, returns and
/// tabs as `\n`, `\r` and `\t`, other control characters in octal.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_ascii_control() => {
                let _ = write!(escaped, "\\{:03o}", u32::from(ch));
            }
            ch => escaped.push(ch),
        }
    }
    escaped
}

/// The English strings catalog, NUL-terminated, in `NeXTStep` `.strings`
/// format. It is static: built on the first call and never freed, as
/// `papplSystemAddStringsData` requires.
#[unsafe(no_mangle)]
pub extern "C" fn pm_strings_en() -> *const c_char {
    EN.text.as_ptr()
}

/// The English text for `key` in [`pm_strings_en`]'s catalog, or NULL if
/// it has none, or `key` is NULL. The string is static.
///
/// # Safety
///
/// `key` must be NULL or point to a NUL-terminated string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn pm_string_en(key: *const c_char) -> *const c_char {
    if key.is_null() {
        return ptr::null();
    }
    // SAFETY: the caller passes a NUL-terminated string.
    let key = unsafe { CStr::from_ptr(key) };
    EN.get(key).map_or(ptr::null(), CStr::as_ptr)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The pairs of a `.strings` file, read as PAPPL 1.4's `loc.c` reads
    /// them: `"key" = "text";` with backslash escapes, the character after
    /// each `;` skipped, and no comments, which it misreads.
    fn parse(mut text: &str) -> Vec<(String, String)> {
        fn quoted(text: &str) -> (String, &str) {
            let mut chars = text.strip_prefix('"').expect("a quote").char_indices();
            let mut value = String::new();
            while let Some((index, ch)) = chars.next() {
                match ch {
                    '"' => return (value, &text[index + 2..]),
                    '\\' => {
                        let (_, escaped) = chars.next().expect("an escape");
                        match escaped {
                            'n' => value.push('\n'),
                            'r' => value.push('\r'),
                            't' => value.push('\t'),
                            '0'..='3' => {
                                let digits: String = std::iter::once(escaped)
                                    .chain(chars.by_ref().take(2).map(|(_, ch)| ch))
                                    .collect();
                                let code = u32::from_str_radix(&digits, 8).expect("octal");
                                value.push(char::from_u32(code).expect("a char"));
                            }
                            other => value.push(other),
                        }
                    }
                    ch => value.push(ch),
                }
            }
            panic!("unterminated string");
        }

        let mut pairs = Vec::new();
        loop {
            text = text.trim_start();
            if text.is_empty() {
                return pairs;
            }
            let (key, rest) = quoted(text);
            let rest = rest.trim_start().strip_prefix('=').expect("=").trim_start();
            let (value, rest) = quoted(rest);
            let rest = rest.strip_prefix(';').expect(";");
            // loc.c's loop skips the next character unread, which must be
            // there (else it steps over the NUL) and white space.
            let mut skipped = rest.chars();
            assert!(
                skipped.next().is_some_and(char::is_whitespace),
                "a pair not followed by white space: {rest:?}"
            );
            text = skipped.as_str();
            pairs.push((key, value));
        }
    }

    fn en_text() -> &'static str {
        // SAFETY: pm_strings_en returns a static C string.
        unsafe { CStr::from_ptr(pm_strings_en()) }
            .to_str()
            .expect("UTF-8")
    }

    #[test]
    fn the_english_catalog() {
        let text = en_text();
        for line in [
            r#""media.om_40x30mm-overprint-2mm_44x34mm" = "40 x 30 mm + 2 mm overprint";"#,
            r#""phomemo-overprint-vertical" = "Overprint (vertical)";"#,
            r#""phomemo-overprint-vertical.clip" = "Label only (top and bottom bleed not printed)";"#,
            r#""phomemo-overprint-vertical.trailing" = "Label and bottom bleed (top bleed not printed)";"#,
            r#""phomemo-dither" = "Dithering";"#,
            r#""phomemo-compression" = "Compression";"#,
        ] {
            assert!(
                text.lines().any(|candidate| candidate == line),
                "{line}\n{text}"
            );
        }
        assert!(text.ends_with('\n'), "{text:?}");
        // Exactly those, each key once, every line a pair: no comments.
        assert!(
            text.lines()
                .all(|line| line.starts_with('"') && line.ends_with("\";")),
            "{text}"
        );
        let pairs = parse(text);
        assert_eq!(pairs.len(), 6, "{text}");
        let mut keys: Vec<_> = pairs.iter().map(|(key, _)| key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), pairs.len());
        assert_eq!(pairs, en_pairs());
    }

    #[test]
    fn every_canvas_and_policy_is_named() {
        let pairs = parse(en_text());
        let has = |key: &str| pairs.iter().any(|(candidate, _)| candidate == key);
        for model in Model::all() {
            for profile in model.overprint_profiles() {
                assert!(has(&format!("media.{}", profile.canvas_name())));
            }
        }
        for policy in VerticalPolicy::ALL {
            assert!(has(&format!(
                "phomemo-overprint-vertical.{}",
                policy.name()
            )));
        }
    }

    #[test]
    fn escaping_survives_a_round_trip() {
        let pairs = vec![
            ("plain".to_owned(), "40 x 30 mm".to_owned()),
            ("quote\"key".to_owned(), "say \"hi\"".to_owned()),
            ("back\\slash".to_owned(), "C:\\path\\".to_owned()),
            ("controls".to_owned(), "a\nb\rc\td\u{1}e\u{7f}".to_owned()),
            ("unicode".to_owned(), "40 × 30 mm".to_owned()),
        ];
        let catalog = Catalog::new(&pairs);
        let text = catalog.text.to_str().expect("UTF-8");
        assert!(text.ends_with('\n'), "{text:?}");
        assert_eq!(parse(text), pairs, "{text}");
        assert!(text.contains(r#""quote\"key" = "say \"hi\"";"#), "{text}");
        assert!(text.contains(r#""back\\slash" = "C:\\path\\";"#), "{text}");
        assert!(text.contains(r#""a\nb\rc\td\001e\177""#), "{text}");
    }

    #[test]
    fn pairs_with_a_nul_are_left_out() {
        let catalog = Catalog::new(&[
            ("bad\0key".to_owned(), "x".to_owned()),
            ("good".to_owned(), "bad\0text".to_owned()),
            ("kept".to_owned(), "yes".to_owned()),
        ]);
        assert_eq!(
            parse(catalog.text.to_str().expect("UTF-8")),
            [("kept".to_owned(), "yes".to_owned())]
        );
    }

    #[test]
    fn lookup_by_key() {
        // SAFETY: static C strings and NULL are valid arguments; the
        // results are static C strings or NULL.
        unsafe {
            let clip = pm_string_en(c"phomemo-overprint-vertical.clip".as_ptr());
            assert_eq!(
                CStr::from_ptr(clip),
                c"Label only (top and bottom bleed not printed)"
            );
            assert!(pm_string_en(c"phomemo-overprint-vertical.full".as_ptr()).is_null());
            assert!(pm_string_en(ptr::null()).is_null());
        }
        // The same text every call: PAPPL keeps the pointer.
        assert!(ptr::eq(pm_strings_en(), pm_strings_en()));
    }
}
