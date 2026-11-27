use std::{collections::BTreeSet, ops::Range, sync::Mutex, time::Duration};
use unicode_segmentation::UnicodeSegmentation;

pub(crate) const DELAY: Duration = Duration::from_millis(400);
pub(crate) const LONGEST: usize = 20_000;
pub(crate) const GUESSES: usize = 5;

struct Accepted {
    ignored: BTreeSet<String>,
    learned: BTreeSet<String>,
    generation: u64,
}

static ACCEPTED: Mutex<Accepted> = Mutex::new(Accepted { ignored: BTreeSet::new(), learned: BTreeSet::new(), generation: 0 });

fn accepted_words<R>(look: impl FnOnce(&mut Accepted) -> R) -> R {
    look(&mut ACCEPTED.lock().unwrap_or_else(|e| e.into_inner()))
}

fn standing_in() -> bool {
    cfg!(test) || crate::acceptance::replaying() || !cfg!(target_os = "macos")
}

fn folded(word: &str) -> String {
    word.to_lowercase()
}

pub(crate) fn accepted(word: &str) -> bool {
    let word = folded(word);
    accepted_words(|accepted| accepted.ignored.contains(&word) || accepted.learned.contains(&word))
}

pub(crate) fn generation() -> u64 {
    accepted_words(|accepted| accepted.generation)
}

pub(crate) fn ignore(word: &str) {
    accepted_words(|accepted| {
        accepted.ignored.insert(folded(word));
        accepted.generation += 1;
    });
}

pub(crate) fn learn(word: &str) {
    if !standing_in() {
        system::learn(word);
    }
    accepted_words(|accepted| {
        accepted.learned.insert(folded(word));
        accepted.generation += 1;
    });
}

#[cfg(test)]
pub(crate) fn learned() -> Vec<String> {
    accepted_words(|accepted| accepted.learned.iter().cloned().collect())
}

fn cut(text: &str) -> &str {
    let mut end = text.len().min(LONGEST);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn code_spans(text: &str) -> Vec<Range<usize>> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in text.split('\n') {
        let ticks: Vec<usize> = line.match_indices('`').map(|(at, _)| at).collect();
        for pair in ticks.chunks_exact(2) {
            spans.push(offset + pair[0]..offset + pair[1] + 1);
        }
        offset += line.len() + 1;
    }
    spans
}

fn code_like(word: &str) -> bool {
    let mut lower_before = false;
    for c in word.chars() {
        if c.is_uppercase() && lower_before {
            return true;
        }
        if c.is_ascii_digit() || c == '_' {
            return true;
        }
        lower_before = c.is_lowercase();
    }
    false
}

pub(crate) fn kept(text: &str, found: Vec<Range<usize>>) -> Vec<Range<usize>> {
    let mut skipped = crate::view::commit_input::links(text);
    skipped.extend(code_spans(text));
    found
        .into_iter()
        .filter(|range| range.start < range.end && range.end <= text.len() && text.is_char_boundary(range.start) && text.is_char_boundary(range.end))
        .filter(|range| !skipped.iter().any(|skip| skip.start < range.end && range.start < skip.end))
        .filter(|range| !code_like(&text[range.clone()]) && !accepted(&text[range.clone()]))
        .collect()
}

pub(crate) fn misspelt(text: &str) -> Vec<Range<usize>> {
    let text = cut(text);
    if text.trim().is_empty() {
        return Vec::new();
    }
    let found = if standing_in() { stand_in::misspelt(text) } else { system::misspelt(text) };
    kept(text, found)
}

pub(crate) fn guesses(text: &str, range: Range<usize>) -> Vec<String> {
    let Some(word) = text.get(range.clone()) else { return Vec::new() };
    let mut guesses = if standing_in() { stand_in::guesses(word) } else { system::guesses(text, range) };
    guesses.retain(|guess| !guess.is_empty() && guess != word);
    guesses.dedup();
    guesses.truncate(GUESSES);
    guesses
}

pub(crate) fn utf16_to_bytes(text: &str, units: Range<usize>) -> Range<usize> {
    let byte = |wanted: usize| {
        let mut used = 0;
        for (index, c) in text.char_indices() {
            if used >= wanted {
                return index;
            }
            used += c.len_utf16();
        }
        text.len()
    };
    byte(units.start)..byte(units.end)
}

pub(crate) fn bytes_to_utf16(text: &str, bytes: Range<usize>) -> Range<usize> {
    text[..bytes.start].encode_utf16().count()..text[..bytes.end].encode_utf16().count()
}

pub(crate) fn moved(marks: &[Range<usize>], before: &str, after: &str) -> Vec<Range<usize>> {
    let prefix = before.bytes().zip(after.bytes()).take_while(|(a, b)| a == b).count();
    let room = before.len().min(after.len()) - prefix;
    let suffix = before.bytes().rev().zip(after.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
    let changed_end = before.len() - suffix;
    let grown = after.len() as isize - before.len() as isize;
    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '\'' || c == '_');
    let shift = |at: usize| (at as isize + grown) as usize;
    marks
        .iter()
        .filter_map(|mark| {
            let before_edit = mark.end < prefix || (mark.end == prefix && !word(after[mark.end..].chars().next()));
            let after_edit = mark.start > changed_end
                || (mark.start == changed_end && mark.start > 0 && !word(after[..shift(mark.start)].chars().next_back()));
            if before_edit {
                Some(mark.clone())
            } else if after_edit {
                Some(shift(mark.start)..shift(mark.end))
            } else {
                None
            }
        })
        .filter(|mark| after.is_char_boundary(mark.start) && after.is_char_boundary(mark.end))
        .collect()
}

mod stand_in {
    use super::*;

    const KNOWN: &[(&str, &[&str])] = &[
        ("teh", &["the", "ten", "tea"]),
        ("recieve", &["receive"]),
        ("brigde", &["bridge", "bride"]),
        ("lantren", &["lantern"]),
        ("ferrry", &["ferry"]),
    ];

    fn entry(word: &str) -> Option<&'static [&'static str]> {
        let word = folded(word);
        KNOWN.iter().find(|(known, _)| *known == word).map(|(_, guesses)| *guesses)
    }

    pub(super) fn misspelt(text: &str) -> Vec<Range<usize>> {
        text.split_word_bound_indices()
            .filter(|(_, word)| entry(word).is_some())
            .map(|(at, word)| at..at + word.len())
            .collect()
    }

    pub(super) fn guesses(word: &str) -> Vec<String> {
        let capital = word.chars().next().is_some_and(char::is_uppercase);
        entry(word)
            .unwrap_or_default()
            .iter()
            .map(|guess| {
                if capital {
                    let mut chars = guess.chars();
                    chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
                } else {
                    (*guess).to_owned()
                }
            })
            .collect()
    }
}

#[cfg(target_os = "macos")]
mod system {
    use super::*;
    use std::ffi::{CStr, c_char, c_void};

    type Object = *mut c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct NsRange {
        location: usize,
        length: usize,
    }

    const NOT_FOUND: usize = isize::MAX as usize;
    const UTF8: usize = 4;
    const MOST_FOUND: usize = 500;

    #[link(name = "AppKit", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> Object;
        fn sel_registerName(name: *const c_char) -> Object;
        fn objc_msgSend();
        fn objc_autoreleasePoolPush() -> *mut c_void;
        fn objc_autoreleasePoolPop(pool: *mut c_void);
    }

    fn pooled<R>(work: impl FnOnce() -> R) -> R {
        unsafe {
            let pool = objc_autoreleasePoolPush();
            let result = work();
            objc_autoreleasePoolPop(pool);
            result
        }
    }

    unsafe fn selector(name: &CStr) -> Object {
        unsafe { sel_registerName(name.as_ptr()) }
    }

    unsafe fn send(receiver: Object, name: &CStr) -> Object {
        unsafe {
            let send: unsafe extern "C" fn(Object, Object) -> Object = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            send(receiver, selector(name))
        }
    }

    unsafe fn checker() -> Object {
        unsafe {
            let class = objc_getClass(c"NSSpellChecker".as_ptr());
            if class.is_null() {
                return std::ptr::null_mut();
            }
            send(class, c"sharedSpellChecker")
        }
    }

    unsafe fn string(text: &str) -> Object {
        unsafe {
            let class = objc_getClass(c"NSString".as_ptr());
            if class.is_null() {
                return std::ptr::null_mut();
            }
            let made = send(class, c"alloc");
            let init: unsafe extern "C" fn(Object, Object, *const u8, usize, usize) -> Object =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let made = init(made, selector(c"initWithBytes:length:encoding:"), text.as_ptr(), text.len(), UTF8);
            if made.is_null() {
                return made;
            }
            send(made, c"autorelease")
        }
    }

    unsafe fn text_of(string: Object) -> Option<String> {
        unsafe {
            if string.is_null() {
                return None;
            }
            let utf8: unsafe extern "C" fn(Object, Object) -> *const c_char = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let bytes = utf8(string, selector(c"UTF8String"));
            (!bytes.is_null()).then(|| CStr::from_ptr(bytes).to_string_lossy().into_owned())
        }
    }

    pub(super) fn misspelt(text: &str) -> Vec<Range<usize>> {
        pooled(|| unsafe {
            let checker = checker();
            let string = string(text);
            if checker.is_null() || string.is_null() {
                return Vec::new();
            }
            let length: unsafe extern "C" fn(Object, Object) -> usize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let units = length(string, selector(c"length"));
            let check: unsafe extern "C" fn(Object, Object, Object, isize, Object, bool, isize, *mut isize) -> NsRange =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let picked = selector(c"checkSpellingOfString:startingAt:language:wrap:inSpellDocumentWithTag:wordCount:");
            let mut found = Vec::new();
            let mut from = 0;
            while from < units && found.len() < MOST_FOUND {
                let range = check(checker, picked, string, from as isize, std::ptr::null_mut(), false, 0, std::ptr::null_mut());
                if range.location == NOT_FOUND || range.length == 0 || range.location < from || range.location >= units {
                    break;
                }
                found.push(utf16_to_bytes(text, range.location..range.location + range.length));
                from = range.location + range.length;
            }
            found
        })
    }

    pub(super) fn guesses(text: &str, range: Range<usize>) -> Vec<String> {
        pooled(|| unsafe {
            let checker = checker();
            let string = string(text);
            if checker.is_null() || string.is_null() {
                return Vec::new();
            }
            let units = bytes_to_utf16(text, range);
            let word = NsRange { location: units.start, length: units.end - units.start };
            let guess: unsafe extern "C" fn(Object, Object, NsRange, Object, Object, isize) -> Object =
                std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let list = guess(
                checker,
                selector(c"guessesForWordRange:inString:language:inSpellDocumentWithTag:"),
                word,
                string,
                std::ptr::null_mut(),
                0,
            );
            if list.is_null() {
                return Vec::new();
            }
            let count: unsafe extern "C" fn(Object, Object) -> usize = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let at: unsafe extern "C" fn(Object, Object, usize) -> Object = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            let total = count(list, selector(c"count")).min(GUESSES * 2);
            (0..total).filter_map(|index| text_of(at(list, selector(c"objectAtIndex:"), index))).collect()
        })
    }

    pub(super) fn learn(word: &str) {
        pooled(|| unsafe {
            let checker = checker();
            let string = string(word);
            if checker.is_null() || string.is_null() {
                return;
            }
            let learn: unsafe extern "C" fn(Object, Object, Object) = std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
            learn(checker, selector(c"learnWord:"), string);
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod system {
    use super::*;

    pub(super) fn misspelt(_: &str) -> Vec<Range<usize>> {
        Vec::new()
    }

    pub(super) fn guesses(_: &str, _: Range<usize>) -> Vec<String> {
        Vec::new()
    }

    pub(super) fn learn(_: &str) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<&str> {
        misspelt(text).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn tests_check_with_the_stand_in_and_never_the_system_dictionary() {
        assert!(standing_in());
        assert_eq!(words("Fix teh brigde over the ford"), ["teh", "brigde"]);
        assert_eq!(words("Teh lantern"), ["Teh"]);
        assert!(words("").is_empty() && words("   \n").is_empty());
        assert_eq!(guesses("Fix teh ford", 4..7), ["the", "ten", "tea"]);
        assert_eq!(guesses("Teh ford", 0..3), ["The", "Ten", "Tea"]);
        assert!(guesses("Fix the ford", 4..7).is_empty());
        assert!(guesses("short", 3..40).is_empty());
    }

    #[test]
    fn links_code_and_identifiers_are_not_marked() {
        let text = "Teh `teh` fix for https://example.invalid/teh and CommitInput brigde_2";
        let at = |word: &str, from: usize| {
            let start = from + text[from..].find(word).unwrap();
            start..start + word.len()
        };
        let found = vec![at("Teh", 0), at("teh", 1), at("teh", 30), at("CommitInput", 0), at("brigde_2", 0)];
        assert_eq!(kept(text, found).into_iter().map(|range| &text[range]).collect::<Vec<_>>(), ["Teh"]);
        assert!(kept("ab", vec![1..9, 2..1]).is_empty());
        assert!(kept("é", vec![1..2]).is_empty());
        assert_eq!(code_spans("a `b` c `d\n` e"), [2..5]);
        assert!(code_like("parseConfig") && code_like("v2") && code_like("snake_case"));
        assert!(!code_like("Lantern") && !code_like("NASA") && !code_like("ferry"));
    }

    #[test]
    fn learning_and_ignoring_are_remembered_and_learning_stays_in_the_stand_in() {
        let before = generation();
        assert_eq!(words("A lantren by the ferrry"), ["lantren", "ferrry"]);
        ignore("Lantren");
        assert!(generation() > before);
        assert_eq!(words("A lantren by the ferrry"), ["ferrry"]);
        learn("ferrry");
        assert!(words("A lantren by the ferrry").is_empty());
        assert!(learned().contains(&"ferrry".to_owned()));
        assert!(!learned().contains(&"lantren".to_owned()));
        assert!(accepted("FERRRY") && accepted("lantren") && !accepted("recieve"));
    }

    #[test]
    fn ranges_cross_between_utf16_units_and_bytes() {
        let text = "café 😀 teh";
        let bytes = text.find("teh").unwrap();
        let units = bytes_to_utf16(text, bytes..bytes + 3);
        assert_eq!(units, 8..11);
        assert_eq!(utf16_to_bytes(text, units), bytes..bytes + 3);
        assert_eq!(utf16_to_bytes(text, 0..99), 0..text.len());
        let long = "x".repeat(LONGEST + 10);
        assert_eq!(cut(&long).len(), LONGEST);
        let wide = "é".repeat(LONGEST);
        assert!(cut(&wide).len() <= LONGEST && wide.is_char_boundary(cut(&wide).len()));
    }

    #[test]
    fn marks_outside_an_edit_survive_and_shift_while_marks_it_touches_go() {
        let before = "teh brigde and teh ferrry";
        let marks = vec![0..3, 4..10, 15..18, 19..25];
        let after = "teh brigde over and teh ferrry";
        assert_eq!(moved(&marks, before, after), vec![0..3, 4..10, 20..23, 24..30]);
        let typed = "teh brigdes and teh ferrry";
        assert_eq!(moved(&marks, before, typed), vec![0..3, 16..19, 20..26]);
        assert_eq!(moved(&marks, before, before), marks);
        assert!(moved(&marks, before, "").is_empty());
        assert_eq!(moved(&[4..7], "a b teh", "a b teh c"), vec![4..7]);
    }
}
