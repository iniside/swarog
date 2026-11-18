use super::*;

/// Every variant of every enum `controlplane::declared_enums` registers as drawn is converted
/// into a view model by some golden case, and no undrawn or undeclared enum is.
#[test]
fn golden_renders_convert_every_drawn_enum_variant() {
    let env = render::env().expect("templates compile");
    let record = seen_in(&cases(&env));
    if let Err(problems) = check_declared(controlplane::declared_enums(), &record) {
        panic!("declared enums against the golden renders:\n  {}", problems.join("\n  "));
    }
}

/// The union of what each of `cases` converted while it alone was built and rendered.
fn seen_in(cases: &[Case]) -> render::seen::Record {
    let mut out = render::seen::Record::new();
    for (name, converted) in cases.iter().flat_map(|c| &c.seen) {
        out.entry(*name)
            .or_insert_with(|| render::seen::Converted { declared: converted.declared, converted: BTreeSet::new() })
            .converted
            .extend(&converted.converted);
    }
    out
}

/// Every problem of `record` against `declared`: no drawn enum declared, each variant of a
/// drawn enum no case converted, each drawn enum no case converted at all, each undrawn enum a
/// case converted, and each converted enum not declared.
fn check_declared(declared: &[DeclaredEnum], record: &render::seen::Record) -> Result<(), Vec<String>> {
    if !declared.iter().any(|e| e.drawn) {
        let empty = "controlplane::declared_enums() holds no drawn enum: no variants! entry is linked";
        return Err(vec![empty.to_string()]);
    }
    let mut declared: Vec<&DeclaredEnum> = declared.iter().collect();
    declared.sort_by_key(|e| e.name);
    let mut problems = Vec::new();
    for e in &declared {
        match record.get(e.name) {
            Some(_) if !e.drawn => {
                problems.push(format!("{} is declared undrawn but the render converted it", e.name));
            }
            None if !e.drawn => {}
            None => {
                let all: Vec<String> = e.variants.iter().map(|v| format!("{}::{v}", e.name)).collect();
                problems.push(format!("{} is converted by no golden case: {}", e.name, all.join(", ")));
            }
            Some(seen) => {
                for v in e.variants.iter().filter(|v| !seen.converted.contains(*v)) {
                    problems.push(format!("{}::{v} is converted by no golden case", e.name));
                }
            }
        }
    }
    for name in record.keys().filter(|n| !declared.iter().any(|e| e.name == **n)) {
        problems.push(format!("{name} is converted but not declared through variants!"));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// A record short one variant, one missing a drawn enum, one holding an undrawn enum, one
/// holding an undeclared enum, and a registry with no drawn enum each fail, naming the enum.
#[test]
fn check_declared_names_each_unconverted_variant_and_misdrawn_enum() {
    let declared = [
        DeclaredEnum { name: "Tone", variants: &["Ok", "Warn"], drawn: true },
        DeclaredEnum { name: "Axis", variants: &["Left", "Right"], drawn: true },
        DeclaredEnum { name: "FilterKind", variants: &["Text"], drawn: false },
    ];
    let entry = |e: &DeclaredEnum, converted: &[&'static str]| {
        (e.name, render::seen::Converted { declared: e.variants, converted: converted.iter().copied().collect() })
    };
    let full =
        render::seen::Record::from([entry(&declared[0], &["Ok", "Warn"]), entry(&declared[1], &["Left", "Right"])]);
    assert_eq!(check_declared(&declared, &full), Ok(()));

    let short =
        render::seen::Record::from([entry(&declared[0], &["Ok"]), entry(&declared[1], &["Left", "Right"])]);
    assert_eq!(check_declared(&declared, &short), Err(vec!["Tone::Warn is converted by no golden case".to_string()]));

    let no_axis = render::seen::Record::from([entry(&declared[0], &["Ok", "Warn"])]);
    assert_eq!(
        check_declared(&declared, &no_axis),
        Err(vec!["Axis is converted by no golden case: Axis::Left, Axis::Right".to_string()])
    );

    let mut undrawn = full.clone();
    undrawn.extend([entry(&declared[2], &["Text"])]);
    assert_eq!(
        check_declared(&declared, &undrawn),
        Err(vec!["FilterKind is declared undrawn but the render converted it".to_string()])
    );

    let mut extra = full.clone();
    extra.extend([entry(&DeclaredEnum { name: "Fresh", variants: &["A"], drawn: true }, &["A"])]);
    assert_eq!(
        check_declared(&declared, &extra),
        Err(vec!["Fresh is converted but not declared through variants!".to_string()])
    );

    assert_eq!(
        check_declared(&declared[2..], &full),
        Err(vec!["controlplane::declared_enums() holds no drawn enum: no variants! entry is linked".to_string()])
    );
}

/// Every `.rs` file under `core/controlplane/src`, read from disk at test time, as (path
/// relative to that directory, source).
fn controlplane_sources() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("controlplane").join("src");
    let mut out = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display())) {
            let path = entry.expect("controlplane src entry").path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path.strip_prefix(&root).expect("under src").to_string_lossy().replace('\\', "/");
                out.push((rel, fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))));
            }
        }
    }
    out.sort();
    out
}

/// One line per `pub enum` opening a line of `files` outside a `variants! { drawn; .. }` or
/// `variants! { undrawn; .. }` invocation, naming the enum and where it is.
fn undeclared_enums(files: &[(String, String)]) -> Vec<String> {
    let open = "variants! {";
    let mut out = Vec::new();
    for (path, source) in files {
        let mut spans = Vec::new();
        for (at, _) in source.match_indices(open) {
            let start = at + open.len();
            let rest = &source[start..];
            let form = rest.trim_start();
            if !(form.starts_with("drawn;") || form.starts_with("undrawn;")) {
                continue;
            }
            let mut depth = 1usize;
            for (i, c) in rest.char_indices() {
                match c {
                    '{' => depth += 1,
                    '}' => {
                        depth -= 1;
                        if depth == 0 {
                            spans.push(start..start + i);
                            break;
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut offset = 0;
        for (n, line) in source.split_inclusive('\n').enumerate() {
            if let Some(rest) = line.trim_start().strip_prefix("pub enum ") {
                let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if !spans.iter().any(|s| s.contains(&offset)) {
                    out.push(format!(
                        "pub enum {name} at {path}:{} is not declared through variants! {{ drawn; | undrawn; .. }}",
                        n + 1
                    ));
                }
            }
            offset += line.len();
        }
    }
    out
}

/// Every `pub enum` of every file of `core/controlplane/src` is declared drawn or undrawn.
#[test]
fn every_controlplane_enum_is_declared_drawn_or_undrawn() {
    let files = controlplane_sources();
    assert!(files.iter().any(|(path, _)| path == "lib.rs"), "controlplane's lib.rs is read: {files:?}");
    let undeclared = undeclared_enums(&files);
    assert!(undeclared.is_empty(), "undeclared controlplane enums:\n  {}", undeclared.join("\n  "));
}

/// A bare `pub enum` and one inside a `variants!` with no form each fail, naming the enum; one
/// inside `drawn;` or `undrawn;`, its variants carrying braces, passes.
#[test]
fn undeclared_enums_names_a_bare_or_formless_enum() {
    let source = concat!(
        "variants! {\n    drawn;\n    pub enum A {\n        X { a: u8 },\n    }\n}\n",
        "pub enum B {\n    Y,\n}\n",
        "variants! {\n    pub enum C {\n        Z,\n    }\n}\n",
        "variants! {\n    undrawn;\n    pub enum D {\n        W(u8),\n    }\n}\n",
    );
    assert_eq!(
        undeclared_enums(&[("x.rs".to_string(), source.to_string())]),
        [
            "pub enum B at x.rs:7 is not declared through variants! { drawn; | undrawn; .. }",
            "pub enum C at x.rs:11 is not declared through variants! { drawn; | undrawn; .. }",
        ]
    );
}
