//! Spell-checking what is being typed, in every language this machine
//! has a dictionary for.
//!
//! Hunspell dictionaries, read with `spellbook`, which is pure Rust --
//! so there is no C library to link, nothing to add to the `.deb` but a
//! recommendation, and no `unsafe` anywhere near the workspace's ban on
//! it. A machine with no dictionaries installed simply gets no
//! underlines, which is why nothing here reports an error.
//!
//! **One dictionary per language, not per locale.** A system with five
//! English variants installed does not want five copies of English in
//! memory, and checking against all of them would accept both "colour"
//! and "color" everywhere. The locale's own variant wins; failing that,
//! the one whose region repeats the language, which is right for
//! `ru_RU`, `de_DE` and most others; failing that, the first in order.
//!
//! A word is wrong only when **no** dictionary knows it. That is what
//! makes a message with Russian and English in one sentence -- which is
//! most messages here -- check correctly in both.

use mm_md::spell::Word;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where Debian and Ubuntu keep them. Both are read: `myspell` is where
/// older dictionary packages land, and some are only there.
const DICTIONARY_DIRS: &[&str] = &["/usr/share/hunspell", "/usr/share/myspell/dicts"];

pub(crate) struct Spelling {
    /// Kept with the code they came from so a failure to find anything
    /// can be reported in words rather than as silence.
    loaded: Vec<(String, spellbook::Dictionary)>,
    /// Words this reader has said are words.
    ///
    /// A chat client for a team is full of names no dictionary has --
    /// services, ticket prefixes, hostnames -- and underlining every
    /// one of them turns the whole feature into noise somebody turns
    /// off. Held here rather than written into a Hunspell dictionary:
    /// `spellbook` takes its input as text at construction, so adding
    /// a word would mean rebuilding three and a half megabytes of
    /// Russian.
    personal: BTreeSet<String>,
}

impl Spelling {
    /// Read the dictionaries off the disk. Slow -- `ru_RU.dic` alone is
    /// three and a half megabytes -- so never on the main thread.
    #[must_use]
    pub(crate) fn load() -> Self {
        let mut loaded = Vec::new();
        for (code, stem) in chosen(&available()) {
            let Ok(aff) = std::fs::read_to_string(stem.with_extension("aff")) else {
                continue;
            };
            let Ok(dic) = std::fs::read_to_string(stem.with_extension("dic")) else {
                continue;
            };
            if let Ok(dictionary) = spellbook::Dictionary::new(&aff, &dic) {
                loaded.push((code, dictionary));
            }
        }
        Self {
            loaded,
            personal: read_personal(),
        }
    }

    /// The language codes in use, for the status line.
    #[must_use]
    pub(crate) fn languages(&self) -> Vec<&str> {
        self.loaded.iter().map(|(code, _)| code.as_str()).collect()
    }

    /// Wrong in every language this machine knows.
    #[must_use]
    pub(crate) fn is_misspelled(&self, word: &str) -> bool {
        !self.loaded.is_empty()
            && !self.personal.contains(word)
            && !self
                .loaded
                .iter()
                .any(|(_, dictionary)| dictionary.check(word))
    }

    /// Record a word as a word, here and on disk.
    ///
    /// Best effort on the file: a reader whose home is read-only
    /// still gets the word accepted for this session, which is better
    /// than refusing to accept it at all.
    pub(crate) fn learn(&mut self, word: &str) {
        if word.is_empty() || !self.personal.insert(word.to_owned()) {
            return;
        }
        let Some(file) = personal_path() else { return };
        if let Some(folder) = file.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let mut all: Vec<&str> = self.personal.iter().map(String::as_str).collect();
        all.sort_unstable();
        // Rewritten whole rather than appended: the set is small, the
        // order is then stable, and a half-written append would leave
        // a word split across two lines.
        let _ = std::fs::write(&file, all.join("\n") + "\n");
    }

    /// What the author might have meant, best first, from whichever
    /// dictionaries have an opinion.
    #[must_use]
    pub(crate) fn suggestions(&self, word: &str, limit: usize) -> Vec<String> {
        let mut all = Vec::new();
        for (_, dictionary) in &self.loaded {
            let mut from_one = Vec::new();
            dictionary.suggest(word, &mut from_one);
            for suggestion in from_one {
                if !all.contains(&suggestion) {
                    all.push(suggestion);
                }
            }
        }
        all.truncate(limit);
        all
    }

    /// The misspelled words in a piece of text, with where they sit.
    #[must_use]
    pub(crate) fn mistakes(&self, text: &str) -> Vec<Word> {
        mm_md::spell::words(text)
            .into_iter()
            .filter(|word| self.is_misspelled(&word.text))
            .collect()
    }
}

/// Where the reader's own words live.
///
/// Under `XDG_DATA_HOME` rather than the cache: a word somebody
/// taught this client is not something to throw away to reclaim
/// space.
fn personal_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share"))
        })?;
    Some(base.join("mattermost-rs").join("personal.dic"))
}

/// The words taught so far. One per line, blank lines ignored.
fn read_personal() -> BTreeSet<String> {
    let Some(file) = personal_path() else {
        return BTreeSet::new();
    };
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Every `code -> path stem` pair with both an `.aff` and a `.dic`.
fn available() -> BTreeMap<String, PathBuf> {
    let mut found = BTreeMap::new();
    for dir in DICTIONARY_DIRS {
        let Ok(entries) = std::fs::read_dir(Path::new(dir)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "dic") {
                continue;
            }
            if !path.with_extension("aff").is_file() {
                continue;
            }
            let Some(code) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            // First directory wins, which puts hunspell ahead of myspell.
            found.entry(code.to_owned()).or_insert(path);
        }
    }
    found
}

/// One code per language, by the rule in the module docs.
fn chosen(available: &BTreeMap<String, PathBuf>) -> Vec<(String, PathBuf)> {
    let locale = current_locale();
    let mut by_language: BTreeMap<&str, (&String, &PathBuf)> = BTreeMap::new();
    for (code, path) in available {
        let language = code.split(['_', '-']).next().unwrap_or(code);
        let preferred = *code == locale
            || code.eq_ignore_ascii_case(&format!("{language}_{}", language.to_uppercase()));
        match by_language.get(language) {
            // Already have the locale's own: nothing beats that.
            Some((held, _)) if **held == locale => {}
            Some(_) if !preferred => {}
            _ => {
                by_language.insert(language, (code, path));
            }
        }
    }
    by_language
        .into_values()
        .map(|(code, path)| (code.clone(), path.clone()))
        .collect()
}

/// The language the desktop is in, as a dictionary code: `en_US.UTF-8`
/// is `en_US`.
fn current_locale() -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|value| {
            value
                .split(['.', '@'])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dictionaries this machine actually has, used the way the
    /// composer uses them.
    ///
    /// Guarded rather than asserted into existence: a runner with no
    /// `hunspell-*` package installed has nothing to check against, and
    /// that is the same "no underlines" the window degrades to. What
    /// this pins is the part that could silently rot -- that
    /// `spellbook` parses these particular files, that a word wrong in
    /// one language is still right in another, and that a misspelling
    /// is actually caught.
    #[test]
    fn the_installed_dictionaries_check_both_scripts() {
        let spelling = Spelling::load();
        let languages = spelling.languages().join(",");
        if spelling.loaded.is_empty() {
            return;
        }

        if languages.contains("en") {
            assert!(!spelling.is_misspelled("message"));
            assert!(spelling.is_misspelled("mesage"));
            assert!(
                spelling
                    .suggestions("mesage", 6)
                    .contains(&"message".to_owned())
            );
        }
        if languages.contains("ru") {
            assert!(!spelling.is_misspelled("сообщении"));
            assert!(spelling.is_misspelled("сообщениии"));
        }
        if languages.contains("en") && languages.contains("ru") {
            // The point of loading both: one sentence, two scripts, and
            // nothing underlined but the actual mistake.
            let mistakes: Vec<String> = spelling
                .mistakes("Ошибки в сообщениии and teh wrong word")
                .into_iter()
                .map(|word| word.text)
                .collect();
            assert_eq!(mistakes, ["сообщениии", "teh"]);
        }
    }

    /// One dictionary per language, and the locale's variant wins.
    #[test]
    fn one_variant_per_language_is_chosen() {
        let available: BTreeMap<String, PathBuf> =
            ["en_AU", "en_GB", "en_US", "ru_RU", "de_DE", "pt_BR"]
                .iter()
                .map(|code| ((*code).to_owned(), PathBuf::from(format!("/d/{code}.dic"))))
                .collect();
        let codes: Vec<String> = chosen(&available).into_iter().map(|(c, _)| c).collect();
        // en_US because the locale says so in this session, ru_RU and
        // de_DE because the region repeats the language, pt_BR because
        // it is the only Portuguese there is.
        assert!(codes.contains(&"ru_RU".to_owned()));
        assert!(codes.contains(&"de_DE".to_owned()));
        assert!(codes.contains(&"pt_BR".to_owned()));
        let english: Vec<&String> = codes.iter().filter(|c| c.starts_with("en")).collect();
        assert_eq!(english.len(), 1, "one English, got {english:?}");
    }
}
