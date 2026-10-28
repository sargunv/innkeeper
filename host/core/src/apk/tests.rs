use super::table::build::{Layout, config, table, type_chunk};
use super::xml::build::{Node, V, document};
use super::*;

const VIEW: &str = "android.intent.action.VIEW";
const BROWSABLE: &str = "android.intent.category.BROWSABLE";

fn filter<'a>(data: Vec<Node<'a>>, browsable: bool) -> Node<'a> {
    let mut c = vec![Node("action", vec![("name", V::S(VIEW))], vec![])];
    if browsable {
        c.push(Node("category", vec![("name", V::S(BROWSABLE))], vec![]));
    }
    c.extend(data);
    Node("intent-filter", vec![], c)
}

fn data<'a>(attrs: Vec<(&'a str, V<'a>)>) -> Node<'a> {
    Node("data", attrs, vec![])
}

/// A manifest like aapt2 compiles, with a table holding the label (`7f01`
/// strings: 0 label, 1 label alias, 2 version, 3 scheme), the icon (`7f02`
/// mipmaps: 0 adaptive with legacy bitmaps, 1 alias of 0) and a bool (`7f03`).
fn sample() -> ApkInfo {
    let manifest = document(&Node(
        "manifest",
        vec![
            ("versionCode", V::I(53)),
            ("versionName", V::R(0x7f01_0002)),
            ("package", V::S("com.example.app")),
        ],
        vec![
            Node(
                "uses-permission",
                vec![("name", V::S("android.permission.INTERNET"))],
                vec![],
            ),
            Node(
                "uses-permission",
                vec![
                    ("name", V::S("android.permission.CAMERA")),
                    ("maxSdkVersion", V::I(40)),
                ],
                vec![],
            ),
            Node(
                "uses-permission",
                vec![
                    ("name", V::S("android.permission.GET_ACCOUNTS")),
                    ("maxSdkVersion", V::I(22)),
                ],
                vec![],
            ),
            Node(
                "uses-permission-sdk-23",
                vec![("name", V::S("android.permission.INTERNET"))],
                vec![],
            ),
            Node(
                "application",
                vec![
                    ("label", V::R(0x7f01_0001)),
                    ("icon", V::R(0x7f02_0001)),
                    ("appCategory", V::I(5)),
                ],
                vec![
                    Node(
                        "activity",
                        vec![("name", V::S("com.example.Main"))],
                        vec![
                            filter(
                                vec![
                                    data(vec![("scheme", V::S("https"))]),
                                    data(vec![("host", V::S("*.example.org"))]),
                                    data(vec![("scheme", V::S("exmpl"))]),
                                    data(vec![("scheme", V::R(0x7f01_0003))]),
                                    data(vec![("scheme", V::S("bad;scheme"))]),
                                ],
                                true,
                            ),
                            filter(vec![data(vec![("scheme", V::S("notbrowsable"))])], false),
                        ],
                    ),
                    Node(
                        "activity",
                        vec![("exported", V::B(false))],
                        vec![filter(vec![data(vec![("scheme", V::S("private"))])], true)],
                    ),
                    Node(
                        "activity",
                        vec![("exported", V::R(0x7f03_0000))],
                        vec![filter(vec![data(vec![("scheme", V::S("private2"))])], true)],
                    ),
                    Node(
                        "activity-alias",
                        vec![],
                        vec![filter(
                            vec![
                                data(vec![
                                    ("scheme", V::S("content")),
                                    ("mimeType", V::S("text/plain")),
                                ]),
                                data(vec![("scheme", V::S("typed"))]),
                            ],
                            true,
                        )],
                    ),
                    Node(
                        "receiver",
                        vec![],
                        vec![filter(vec![data(vec![("scheme", V::S("receiver"))])], true)],
                    ),
                ],
            ),
        ],
    ));
    let mut night = config("", "", 0, 0);
    night[29] = 0x20;
    let mut sr_latn = config("sr", "", 0, 0);
    sr_latn[36..40].copy_from_slice(b"Latn");
    let strings = [
        "Calculator",
        "Rechner",
        "Calculadora",
        "Kalkulator",
        "3.2 beta",
        "mycalc",
        "res/m.png",
        "res/x.webp",
        "res/a.xml",
        "Night",
        "res/n.png",
    ];
    let s = |x: &str| strings.iter().position(|y| *y == x).unwrap() as u32;
    let arsc = table(
        &strings,
        0x7f,
        &[
            type_chunk(
                1,
                &config("", "", 0, 0),
                &[
                    (0, 0x03, s("Calculator")),
                    (1, 0x01, 0x7f01_0000),
                    (2, 0x03, s("3.2 beta")),
                    (3, 0x03, s("mycalc")),
                ],
                Layout::Dense,
            ),
            type_chunk(
                1,
                &config("de", "", 0, 0),
                &[(0, 0x03, s("Rechner"))],
                Layout::Sparse,
            ),
            type_chunk(
                1,
                &config("pt", "BR", 0, 0),
                &[(0, 0x03, s("Calculadora"))],
                Layout::Offset16,
            ),
            type_chunk(1, &sr_latn, &[(0, 0x03, s("Kalkulator"))], Layout::Compact),
            // Same as the default: not a translation.
            type_chunk(
                1,
                &config("en", "GB", 0, 0),
                &[(0, 0x03, s("Calculator"))],
                Layout::Dense,
            ),
            type_chunk(1, &night, &[(0, 0x03, s("Night"))], Layout::Dense),
            type_chunk(
                2,
                &config("", "", 160, 0),
                &[(0, 0x03, s("res/m.png"))],
                Layout::Dense,
            ),
            type_chunk(
                2,
                &config("", "", 640, 4),
                &[(0, 0x03, s("res/x.webp"))],
                Layout::Dense,
            ),
            type_chunk(
                2,
                &config("", "", 0xfffe, 26),
                &[(0, 0x03, s("res/a.xml"))],
                Layout::Dense,
            ),
            type_chunk(2, &night, &[(0, 0x03, s("res/n.png"))], Layout::Dense),
            type_chunk(
                2,
                &config("", "", 0, 0),
                &[(1, 0x01, 0x7f02_0000)],
                Layout::Dense,
            ),
            type_chunk(3, &config("", "", 0, 0), &[(0, 0x12, 0)], Layout::Dense),
        ],
    );
    let files: Vec<String> = [
        "lib/x86_64/libc.so",
        "lib/arm64-v8a/libc.so",
        "lib/x86_64/",
        "lib/bad abi/x.so",
        "classes.dex",
    ]
    .map(String::from)
    .into();
    parse(&manifest, Some(&arsc), &files).unwrap()
}

#[test]
fn manifest_fields() {
    let a = sample();
    assert_eq!(a.package, "com.example.app");
    assert_eq!(a.version_code, "53");
    assert_eq!(a.version_name, "3.2 beta");
    assert_eq!(a.split, "");
    assert_eq!(a.native_code, ["arm64-v8a", "x86_64"]);
    assert_eq!(
        a.permissions,
        ["android.permission.CAMERA", "android.permission.INTERNET"]
    );
    assert_eq!(a.app_category, Some(5));
    assert!(!a.is_game);
}

#[test]
fn localized_labels() {
    let a = sample();
    assert_eq!(a.label, "Calculator");
    assert_eq!(
        a.labels,
        [
            ("de".into(), "Rechner".into()),
            ("pt-BR".into(), "Calculadora".into()),
            ("sr-Latn".into(), "Kalkulator".into()),
        ]
    );
}

#[test]
fn icon_variants() {
    // Through the alias; the night variant is left out.
    assert_eq!(
        sample().icons,
        [
            (160, "res/m.png".into()),
            (640, "res/x.webp".into()),
            (0xfffe, "res/a.xml".into())
        ]
    );
}

#[test]
fn links() {
    let a = sample();
    assert_eq!(a.schemes, ["exmpl", "mycalc"]);
    assert_eq!(a.web_hosts, ["*.example.org"]);
}

#[test]
fn without_a_table() {
    let manifest = document(&Node(
        "manifest",
        vec![("package", V::S("a.b")), ("split", V::S("config.x86_64"))],
        vec![Node(
            "application",
            vec![("label", V::R(0x7f01_0000)), ("isGame", V::B(true))],
            vec![],
        )],
    ));
    let a = parse(&manifest, None, &[]).unwrap();
    assert_eq!(a.split, "config.x86_64");
    assert_eq!(a.label, "");
    assert!(a.is_game);
    let not_manifest = document(&Node("resources", vec![], vec![]));
    assert!(parse(&not_manifest, None, &[]).is_err());
}

/// A corpus APK ([`crate::test_corpus`]), or None (the test is skipped) if it
/// isn't fetched.
fn corpus(name: &str) -> Option<ApkInfo> {
    let p = crate::test_corpus().join(name);
    if !p.is_file() {
        eprintln!("skipping: no corpus APK {}", p.display());
        return None;
    }
    Some(read(&p).unwrap())
}

#[test]
fn corpus_wikipedia() {
    let Some(a) = corpus("org.wikipedia_50612.apk") else {
        return;
    };
    assert_eq!(a.package, "org.wikipedia");
    assert_eq!(a.version_code, "50612");
    assert_eq!(a.label, "Wikipedia");
    let label = |tag: &str| {
        a.labels
            .iter()
            .find(|(t, _)| t == tag)
            .map(|(_, l)| l.as_str())
    };
    assert_eq!(label("de"), None, "same as the default");
    assert_eq!(label("ar"), Some("ويكيبيديا"));
    assert_eq!(label("anp"), Some("विकिपीडिया"));
    assert!(a.native_code.iter().any(|c| c == "x86_64"));
    assert_eq!(a.schemes, ["wikipedia"]);
    assert!(
        a.web_hosts.iter().any(|h| h == "*.wikipedia.org"),
        "{:?}",
        a.web_hosts
    );
}

#[test]
fn corpus_opencalc() {
    let Some(a) = corpus("com.darkempire78.opencalculator_53.apk") else {
        return;
    };
    assert_eq!(a.package, "com.darkempire78.opencalculator");
    assert_eq!(a.label, "Calculator");
    let label = |tag: &str| {
        a.labels
            .iter()
            .find(|(t, _)| t == tag)
            .map(|(_, l)| l.as_str())
    };
    assert_eq!(label("de"), Some("Rechner"));
    assert_eq!(label("pt-BR"), Some("Calculadora"));
    // An adaptive icon with legacy bitmaps.
    assert!(
        a.icons
            .iter()
            .any(|(d, p)| *d == 0xfffe && p.ends_with(".xml"))
    );
    assert!(
        a.icons
            .iter()
            .any(|(d, p)| *d == 640 && p.ends_with(".png"))
    );
    assert!(a.schemes.is_empty());
    assert!(a.web_hosts.is_empty());
}

#[test]
fn corpus_thunderbird_schemes() {
    let Some(a) = corpus("net.thunderbird.android_33.apk") else {
        return;
    };
    assert!(a.schemes.iter().all(|s| validate::custom_scheme(s)));
    assert!(a.schemes.iter().any(|s| s == "mailto"), "{:?}", a.schemes);
}

#[test]
fn corpus_game_category() {
    let Some(a) = corpus("com.shatteredpixel.shatteredpixeldungeon_922.apk") else {
        return;
    };
    assert!(a.is_game || a.app_category == Some(0), "{a:?}");
}

#[test]
fn corpus_parses() {
    let dir = crate::test_corpus();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!("skipping: no corpus at {}", dir.display());
        return;
    };
    for e in rd
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "apk"))
    {
        let a = read(&e.path()).unwrap();
        assert!(validate::package(&a.package).is_ok(), "{:?}", e.path());
        assert!(
            validate::label(&a.label).is_some(),
            "{:?}: {:?}",
            e.path(),
            a.label
        );
        assert!(!a.icons.is_empty(), "{:?}", e.path());
    }
}

/// Checks every corpus APK against the SDK's aapt2, through what an install
/// uses: fields, translated labels, the icon picked, schemes and web hosts. Run with
/// `cargo test -p linuxapk-core matches_aapt2 -- --ignored --nocapture`.
#[test]
#[ignore = "needs aapt2 and the corpus"]
fn matches_aapt2() {
    use crate::manifest::{self, Aapt2};
    let aapt = Aapt2::find().unwrap();
    let mut apks: Vec<_> = std::fs::read_dir(crate::test_corpus())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "apk"))
        .collect();
    apks.sort();
    let mut bad = 0;
    for p in &apks {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let b = aapt.badging(p).unwrap();
        let m = aapt.manifest(p).unwrap();
        let a = read(p).unwrap();
        let mut diffs: Vec<String> = vec![];
        let eq = |diffs: &mut Vec<String>, what: &str, old: String, new: String| {
            if old != new {
                diffs.push(format!("{what}: aapt2 {old} / ours {new}"));
            }
        };
        eq(&mut diffs, "package", b.package.clone(), a.package.clone());
        eq(
            &mut diffs,
            "versionCode",
            b.version_code.clone(),
            a.version_code.clone(),
        );
        eq(
            &mut diffs,
            "versionName",
            b.version_name.clone(),
            a.version_name.clone(),
        );
        eq(&mut diffs, "split", b.split.clone(), a.split.clone());
        eq(&mut diffs, "label", b.label.clone(), a.label.clone());
        eq(
            &mut diffs,
            "native",
            format!("{:?}", b.native_code),
            format!("{:?}", a.native_code),
        );
        // aapt2 adds permissions Android implies (READ_ for WRITE_EXTERNAL_STORAGE);
        // we list what the APK declares.
        let raw = std::process::Command::new(crate::manifest::find_aapt2().unwrap())
            .args(["dump", "badging"])
            .arg(p)
            .output()
            .unwrap();
        let implied: Vec<String> = String::from_utf8_lossy(&raw.stdout)
            .lines()
            .filter_map(|l| {
                let n = l.strip_prefix("uses-implied-permission: name='")?;
                Some(n[..n.find('\'')?].to_string())
            })
            .collect();
        let declared: Vec<&String> = b
            .permissions
            .iter()
            .filter(|x| !implied.contains(x))
            .collect();
        let ours: Vec<&String> = a
            .permissions
            .iter()
            .filter(|x| !implied.contains(x))
            .collect();
        eq(
            &mut diffs,
            "permissions",
            format!("{declared:?}"),
            format!("{ours:?}"),
        );
        eq(
            &mut diffs,
            "category",
            format!("{:?}", m.app_category),
            format!("{:?}", a.app_category),
        );
        eq(
            &mut diffs,
            "game",
            m.is_game.to_string(),
            a.is_game.to_string(),
        );
        eq(
            &mut diffs,
            "schemes",
            format!("{:?}", m.schemes),
            format!("{:?}", a.schemes),
        );
        eq(
            &mut diffs,
            "hosts",
            format!("{:?}", m.web_hosts),
            format!("{:?}", a.web_hosts),
        );
        let translated: Vec<_> = b
            .labels
            .iter()
            .filter(|(_, l)| *l != b.label)
            .cloned()
            .collect();
        eq(
            &mut diffs,
            "labels",
            format!("{translated:?}"),
            format!("{:?}", a.labels),
        );
        // The icon an install confirmation shows: the largest bitmap, else
        // the adaptive icon's legacy bitmaps.
        let bitmap = |icons: &[(u32, String)]| {
            icons
                .iter()
                .filter(|(_, p)| p.ends_with(".png") || p.ends_with(".webp"))
                .max_by_key(|(d, _)| *d)
                .map(|(_, p)| p.clone())
        };
        let old_icon = bitmap(&b.icons).or_else(|| {
            let (_, xml) = b.icons.iter().max_by_key(|(d, _)| *d)?;
            bitmap(&manifest::bitmap_variants(&aapt.resources(p).ok()?, xml))
        });
        eq(
            &mut diffs,
            "icon",
            format!("{old_icon:?}"),
            format!("{:?}", bitmap(&a.icons)),
        );
        println!(
            "{name}: {} labels, {} schemes, {} hosts, {} permissions{}",
            a.labels.len(),
            a.schemes.len(),
            a.web_hosts.len(),
            a.permissions.len(),
            if diffs.is_empty() {
                String::new()
            } else {
                format!("\n  DIFF {}", diffs.join("\n  DIFF "))
            }
        );
        bad += usize::from(!diffs.is_empty());
    }
    println!("{} APKs, {bad} with differences", apks.len());
    assert_eq!(bad, 0);
}
