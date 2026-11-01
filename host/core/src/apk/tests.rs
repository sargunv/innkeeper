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

/// A manifest as aapt2 compiles it, with a resource table. The `7f01` strings
/// are 0 label, 1 label alias, 2 version and 3 scheme. The `7f02` mipmaps are
/// 0, an adaptive icon with legacy bitmaps, and 1, an alias of 0. `7f03` holds
/// a bool.
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
            // This matches the default, so it isn't a translation.
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
    // The icon resolves through the alias, and the night variant is left out.
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
