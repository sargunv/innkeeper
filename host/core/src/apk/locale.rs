//! Resource locales, and how Android picks a value for a locale
//! (`ResTable_config::match` and `isLocaleBetterThan`). The language must be
//! the same, and so must the script. When a locale doesn't name a script, its
//! script is the language's usual one. So `kk` matches `b+kk+Cyrl`, `zh-HK`
//! matches `zh-TW` because both are Traditional, and `sr-Latn` doesn't match
//! `sr`.

/// A locale of a resource configuration. An empty field means the qualifier
/// is absent.
#[derive(Debug, Default, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Locale {
    pub lang: String,
    /// Set only when the configuration names a script, as in `b+sr+Latn`.
    pub script: String,
    pub region: String,
    pub variant: String,
}

/// Languages whose usual script isn't Latin, from CLDR's likely subtags. The
/// list covers the most common Android resource languages, and any other
/// language is taken as Latin.
const SCRIPTS: &[(&str, &str)] = &[
    (
        "Cyrl",
        "ab av ba be bg ce cv inh kk krc kum ky mk mn os ru sah sr tg tt tyv udm uk",
    ),
    (
        "Arab",
        "ar arz ary azb bqi ckb fa glk ks lrc mzn pnb ps sd sdh skr ug ur",
    ),
    ("Hebr", "he iw ji yi"),
    (
        "Deva",
        "anp awa bho bra dty gom hi hne kok mag mai mr ne new sa",
    ),
    ("Beng", "as bn bpy mni"),
    ("Mymr", "blk mnw my shn"),
    ("Ethi", "am ti tig"),
    ("Geor", "ka xmf"),
    ("Guru", "pa"),
    ("Gujr", "gu"),
    ("Orya", "or"),
    ("Taml", "ta"),
    ("Telu", "te"),
    ("Knda", "kn tcy"),
    ("Mlym", "ml"),
    ("Sinh", "si"),
    ("Thai", "th"),
    ("Laoo", "lo"),
    ("Khmr", "km"),
    ("Armn", "hy"),
    ("Grek", "el"),
    ("Jpan", "ja"),
    ("Kore", "ko"),
    ("Hans", "zh"),
    ("Hant", "lzh yue"),
    ("Thaa", "dv"),
    ("Tibt", "bo dz"),
    ("Cher", "chr"),
    ("Cans", "cr iu"),
    ("Tfng", "zgh"),
    ("Olck", "sat"),
    ("Nkoo", "nqo"),
];

/// Regions where a language usually takes another script.
const REGION_SCRIPTS: &[(&str, &str, &str)] = &[
    ("zh", "TW", "Hant"),
    ("zh", "HK", "Hant"),
    ("zh", "MO", "Hant"),
    ("pa", "PK", "Arab"),
    ("uz", "AF", "Arab"),
    ("mn", "CN", "Mong"),
    ("az", "IR", "Arab"),
];

impl Locale {
    /// The tag in aapt2's form, such as `de`, `pt-BR`, `sr-Latn` or `es-419`.
    /// It is empty for the default locale.
    pub fn tag(&self) -> String {
        let mut t = self.lang.clone();
        for part in [&self.script, &self.region, &self.variant] {
            if !t.is_empty() && !part.is_empty() {
                t.push('-');
                t.push_str(part);
            }
        }
        t
    }

    pub fn is_default(&self) -> bool {
        self.lang.is_empty()
    }

    /// The named script, or else the language's usual one.
    fn script(&self) -> &str {
        if !self.script.is_empty() {
            return &self.script;
        }
        if let Some((_, _, s)) = REGION_SCRIPTS
            .iter()
            .find(|(l, r, _)| *l == self.lang && *r == self.region)
        {
            return s;
        }
        SCRIPTS
            .iter()
            .find(|(_, langs)| langs.split(' ').any(|l| l == self.lang))
            .map_or("Latn", |(s, _)| s)
    }

    /// `tl` is the old code for `fil`.
    fn same_lang(&self, o: &Locale) -> bool {
        let canon = |l: &str| if l == "tl" { "fil" } else { l }.to_string();
        canon(&self.lang) == canon(&o.lang)
    }

    /// How well a resource in this locale serves `want`. Higher is better, and
    /// None means it can't. The default locale always serves, with the lowest
    /// score.
    pub fn score(&self, want: &Locale) -> Option<u8> {
        if self.is_default() {
            return Some(0);
        }
        if !self.same_lang(want) || self.script() != want.script() {
            return None;
        }
        let region = if self.region == want.region {
            3
        } else if self.region.is_empty() {
            2
        } else {
            1
        };
        Some(region * 2 + u8::from(self.variant == want.variant))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(tag: &str) -> Locale {
        let mut it = tag.split('-');
        let mut loc = Locale {
            lang: it.next().unwrap().into(),
            ..Default::default()
        };
        for p in it {
            if p.len() == 4 {
                loc.script = p.into();
            } else {
                loc.region = p.into();
            }
        }
        loc
    }

    /// The best of `have` for `want`, as a tag.
    fn best(have: &[&str], want: &str) -> String {
        have.iter()
            .map(|t| l(t))
            .filter_map(|c| Some((c.score(&l(want))?, c)))
            .max_by_key(|(s, _)| *s)
            .map(|(_, c)| c.tag())
            .unwrap_or_default()
    }

    #[test]
    fn matching() {
        assert_eq!(l("sr-Latn").tag(), "sr-Latn");
        assert_eq!(best(&["", "de", "de-AT"], "de-AT"), "de-AT");
        assert_eq!(best(&["", "de", "de-AT"], "de-CH"), "de");
        assert_eq!(best(&["", "nb-NO"], "nb"), "nb-NO");
        assert_eq!(best(&["", "fr"], "de"), "");
        assert_eq!(best(&["", "kk-Cyrl"], "kk"), "kk-Cyrl");
        assert_eq!(best(&["", "sr"], "sr-Latn"), "");
        assert_eq!(best(&["", "sr-Latn"], "sr"), "");
        assert_eq!(best(&["", "zh-CN", "zh-TW"], "zh-HK"), "zh-TW");
        assert_eq!(best(&["", "zh-CN", "zh-TW"], "zh"), "zh-CN");
        assert_eq!(best(&["", "tl"], "fil"), "tl");
    }
}
