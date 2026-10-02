use std::{
    borrow::Cow,
    fs,
    path::{Path, PathBuf},
};

use super::*;
use crate::codec;

/// Short and long, single- and multi-byte, and text that JSON must escape.
const SAMPLES: [&str; 8] = [
    "",
    "tone",
    "exactly_twenty_four_byte",
    "twenty_five_bytes_exactly",
    "a question name that is well past the inline limit of a name",
    "\u{8cea}\u{554f}\u{306e}\u{540d}\u{524d}",
    "\u{3068}\u{3066}\u{3082}\u{9577}\u{3044}\u{8cea}\u{554f}\u{306e}\u{540d}",
    "tab\there, a \"quote\", a backslash \\ and a control \u{0001}",
];

#[test]
fn a_name_holds_the_text_it_was_made_from_whatever_it_was_made_from() {
    for text in SAMPLES {
        let from_str = Name::from(text);
        let from_string = Name::from(text.to_owned());
        let from_borrowed = Name::from(Cow::Borrowed(text));
        let from_owned = Name::from(Cow::<str>::Owned(text.to_owned()));

        assert_eq!(from_str.as_str(), text, "from &str: {text:?}");
        assert_eq!(from_string.as_str(), text, "from String: {text:?}");
        assert_eq!(from_borrowed.as_str(), text, "from a borrowed Cow: {text:?}");
        assert_eq!(from_owned.as_str(), text, "from an owned Cow: {text:?}");
        assert_eq!(from_str, from_string, "{text:?}");
        assert_eq!(from_str.clone(), from_owned, "{text:?}");
    }
    assert_ne!(Name::from("tone"), Name::from("tones"));
    assert_ne!(Name::from("exactly_twenty_four_byte"), Name::from("exactly_twenty_four_bytes"));
}

#[test]
fn a_name_prints_and_serializes_as_the_string_it_replaced() {
    for text in SAMPLES {
        let name = Name::from(text);
        let string = text.to_owned();

        assert_eq!(format!("{name:?}"), format!("{string:?}"), "Debug of {text:?}");
        assert_eq!(format!("{name:#?}"), format!("{string:#?}"), "alternate Debug of {text:?}");
        assert_eq!(
            serde_json::to_string(&name).expect("a name serializes"),
            serde_json::to_string(&string).expect("a string serializes"),
            "serde_json of {text:?}"
        );
        let (mut through_codec, mut string_through_codec) = (Vec::new(), Vec::new());
        codec::encode_into(&mut through_codec, &name).expect("a name encodes");
        codec::encode_into(&mut string_through_codec, &string).expect("a string encodes");
        assert_eq!(through_codec, string_through_codec, "the codec's output for {text:?}");
    }
}

#[test]
fn a_name_decodes_from_a_json_string_escaped_or_not() {
    for text in SAMPLES {
        let json = serde_json::to_string(text).expect("a string serializes");
        let through_codec: Name = codec::decode(json.as_bytes()).expect("a JSON string decodes");
        let through_serde_json: Name = serde_json::from_str(&json).expect("a JSON string decodes");
        assert_eq!(through_codec.as_str(), text, "the codec, from {json}");
        assert_eq!(through_serde_json.as_str(), text, "serde_json, from {json}");
    }
}

#[test]
fn a_value_that_is_not_a_string_is_refused_as_a_string_refuses_it() {
    for json in ["12", "null", "true", "[\"a\"]", "{\"a\":1}"] {
        let as_name = codec::decode::<Name>(json.as_bytes()).expect_err("not a string");
        let as_string = codec::decode::<String>(json.as_bytes()).expect_err("not a string");
        assert_eq!(as_name.to_string(), as_string.to_string(), "the error for {json}");
        assert_eq!(as_name.kind(), as_string.kind(), "the error kind for {json}");

        let as_name = serde_json::from_str::<Name>(json).expect_err("not a string");
        let as_string = serde_json::from_str::<String>(json).expect_err("not a string");
        assert_eq!(as_name.to_string(), as_string.to_string(), "serde_json's error for {json}");
    }
}

/// Every file under `dir` that `wanted` accepts, outside `target` and `.git`.
fn files_under(dir: &Path, wanted: &dyn Fn(&Path) -> bool, found: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target" || name == ".git") {
                continue;
            }
            files_under(&path, wanted, found);
        } else if wanted(&path) {
            found.push(path);
        }
    }
}

/// The lines of `path` that `offends` flags, as `path:line: text`.
fn flagged_lines(path: &Path, offends: &dyn Fn(&str) -> bool) -> Vec<String> {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    text.lines()
        .enumerate()
        .filter(|(_, line)| offends(line))
        .map(|(number, line)| format!("{}:{}: {line}", path.display(), number + 1))
        .collect()
}

/// `src/name.rs` is the one file that uses the small-string crate, so that
/// replacing the crate stays a change to that file alone. Three rules keep it
/// so, and each one closes a route the others leave open:
///
/// 1. No Rust file of any workspace member other than `src/name.rs` names the
///    crate: everything under the repository's `crates/`, which holds this
///    package's library, tests, benches and examples and the workspace's
///    other crates, because an integration test or a bench can name a
///    dependency of the library too.
/// 2. Inside `src/name.rs` the crate's type is only the private field of
///    `Name`. Outside comments, the only lines that may name the crate or its
///    type are the private import, the struct declaration and the two
///    constructor calls, exactly as written. Any other line fails: `use ...
///    as`, `pub use` or `pub(crate) use` of anything in the crate, a type
///    alias, or a signature that hands the type out would let another module
///    use the crate without naming it.
/// 3. No `Cargo.toml` of the repository (the workspace root's, every
///    member's, the fuzz crate's) renames the package
///    (`other = { package = "..." }`): a renamed dependency is used under a
///    name the first rule does not look for.
///
/// Both scans are anchored at the repository root, two levels above this
/// package, and assert that they saw named files of every member and every
/// manifest: a layout change that moves one out of reach fails the test
/// instead of narrowing it.
#[test]
fn only_this_module_names_the_small_string_crate() {
    // Spelled in pieces so this file does not name the crate or its type.
    let needle = concat!("compact", "_str");
    let type_name = concat!("Compact", "String");
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    // `ancestors()` yields the path itself first, so `nth(2)` is the grandparent.
    let repository = package.ancestors().nth(2).expect("the package is two levels deep");
    let crates = repository.join("crates");
    let allowed = package.join("src").join("name.rs");

    let is_rust = |path: &Path| path.extension().is_some_and(|extension| extension == "rs");
    let mut files = Vec::new();
    files_under(&crates, &is_rust, &mut files);
    let expected_files = [
        allowed.clone(),
        package.join("tests").join("client.rs"),
        package.join("benches").join("sdk").join("main.rs"),
        package.join("examples").join("quickstart.rs"),
        crates.join("macros").join("src").join("lib.rs"),
        crates.join("test-support").join("src").join("lib.rs"),
        crates.join("live-tests").join("src").join("lib.rs"),
        crates.join("live-tests").join("tests").join("live.rs"),
        crates.join("adapter").join("src").join("lib.rs"),
        crates.join("adapter").join("tests").join("client.rs"),
        crates.join("adapter-live-tests").join("src").join("lib.rs"),
        crates.join("adapter-live-tests").join("tests").join("live.rs"),
    ];
    for expected in &expected_files {
        assert!(files.contains(expected), "the scan did not see {}", expected.display());
    }

    // Rule 1.
    let names_the_crate = |line: &str| line.contains(needle);
    let offenders: Vec<String> = files
        .iter()
        .filter(|path| **path != allowed)
        .flat_map(|path| flagged_lines(path, &names_the_crate))
        .collect();
    assert!(offenders.is_empty(), "only src/name.rs may name {needle}:\n{}", offenders.join("\n"));

    // Rule 2.
    let permitted = [
        format!("use {needle}::{type_name};"),
        format!("pub(crate) struct Name({type_name});"),
        format!("Self({type_name}::from(text))"),
    ];
    let hands_it_out = |line: &str| {
        let code = line.trim();
        !code.starts_with("//")
            && (code.contains(needle) || code.contains(type_name))
            && !permitted.iter().any(|shape| code == shape)
    };
    let exposed = flagged_lines(&allowed, &hands_it_out);
    assert!(
        exposed.is_empty(),
        "src/name.rs may use {type_name} only as Name's private field; these lines go further:\n{}",
        exposed.join("\n")
    );
    let own = fs::read_to_string(&allowed).expect("src/name.rs reads");
    for shape in &permitted {
        assert!(
            own.contains(shape.as_str()),
            "src/name.rs no longer has `{shape}`; update this check"
        );
    }

    // Rule 3. Spaces and the quote style are ignored, so any spelling of the
    // key is caught.
    let is_manifest = |path: &Path| path.file_name().is_some_and(|name| name == "Cargo.toml");
    let mut manifests = Vec::new();
    files_under(repository, &is_manifest, &mut manifests);
    let expected_manifests = [
        repository.join("Cargo.toml"),
        package.join("Cargo.toml"),
        crates.join("macros").join("Cargo.toml"),
        crates.join("test-support").join("Cargo.toml"),
        crates.join("live-tests").join("Cargo.toml"),
        crates.join("adapter").join("Cargo.toml"),
        crates.join("adapter-live-tests").join("Cargo.toml"),
        repository.join("fuzz").join("Cargo.toml"),
    ];
    for expected in &expected_manifests {
        assert!(
            manifests.contains(expected),
            "the scan did not see {}: {manifests:?}",
            expected.display()
        );
    }
    let renamed_to = format!("package=\"{needle}\"");
    let renames_it = |line: &str| {
        let squeezed: String = line
            .chars()
            .filter(|c| !c.is_whitespace())
            .map(|c| if c == '\'' { '"' } else { c })
            .collect();
        squeezed.contains(&renamed_to)
    };
    let renamed: Vec<String> =
        manifests.iter().flat_map(|path| flagged_lines(path, &renames_it)).collect();
    assert!(
        renamed.is_empty(),
        "{needle} is depended on under another name:\n{}",
        renamed.join("\n")
    );
}
