use super::*;

// ---- drift guards ----------------------------------------------------------------

/// The case that renders each `Primitive` variant as a panel body. No wildcard arm: a new
/// variant does not compile until it names its case.
pub(super) fn primitive_case(p: &Primitive) -> &'static str {
    match p {
        Primitive::DataTable(_) => "table_highlight_row_menu",
        Primitive::PropertyList(_) => "state_ok",
        Primitive::KpiTile(_) => "tile_delta_ok_up",
        Primitive::Timeline(_) => "timeline_two_days_older",
        Primitive::Form(_) => "panel_form",
        Primitive::WorkQueue(_) => "queue_items_actions",
        Primitive::ServiceStatus(_) => "services_one_column",
        Primitive::TimeSeriesChart(_) => "chart_two_axes",
        Primitive::AttentionItems(_) => "attention_ok",
        Primitive::ColumnCells(_) => "column_cells",
        Primitive::Cell(_) => "fact_cell",
        Primitive::EntityHeader(_) => "header_ok",
        Primitive::Empty { .. } => "state_empty_payload",
    }
}

/// The case that draws each `PanelState` in panel chrome. No wildcard arm.
pub(super) fn state_case(state: PanelState) -> &'static str {
    match state {
        PanelState::Ok => "state_ok",
        PanelState::Empty => "state_empty",
        PanelState::Stale => "state_stale_hides_module_banner",
        PanelState::Error => "state_error",
        PanelState::Readonly => "state_readonly",
        PanelState::Denied => "state_denied",
    }
}

/// The source `PanelState`'s variant count is parsed from, so a variant added there raises the
/// count the rendered cases must reach; `Primitive`'s comes from `Variants::VARIANTS`.
pub(super) const PANEL_STATE_SOURCE: &str = include_str!("../engine/panels.rs");

/// The variant names of `enum <name>` in `source`, in order: the identifiers that open each
/// top-level item of its body, past comments and attributes.
pub(super) fn enum_variants(source: &str, name: &str) -> Result<Vec<String>, String> {
    let open = format!("enum {name} {{");
    let at = source.find(&open).ok_or_else(|| format!("no `{open}` in the source"))?;
    let chars: Vec<char> = source[at + open.len()..].chars().collect();
    let (mut i, mut depth, mut expect) = (0, 0usize, true);
    let mut out = Vec::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if c == '#' && depth == 0 {
            // An attribute: skip to its closing bracket.
            let mut brackets = 0usize;
            i += 1;
            while i < chars.len() {
                match chars[i] {
                    '[' => brackets += 1,
                    ']' => {
                        brackets -= 1;
                        if brackets == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
            i += 1;
        } else if depth == 0 && expect && (c.is_alphabetic() || c == '_') {
            let start = i;
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                i += 1;
            }
            out.push(chars[start..i].iter().collect());
            expect = false;
        } else {
            match c {
                '(' | '{' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                '}' if depth == 0 => return Ok(out),
                '}' => depth -= 1,
                ',' if depth == 0 => expect = true,
                _ => {}
            }
            i += 1;
        }
    }
    Err(format!("`{open}` is never closed"))
}

/// Fails unless `rendered`, the distinct cases the variant-to-case match names for the
/// variants that render, holds one case per variant of `name`'s `variants`. The match has no
/// wildcard, so a new variant names either a new case, which must render, or an existing one,
/// which leaves the count short.
pub(super) fn check_variant_coverage(name: &str, variants: &[&str], rendered: &BTreeSet<&str>) -> Result<(), String> {
    if variants.len() != rendered.len() {
        return Err(format!(
            "{name} has {} variant(s) {variants:?} but {} distinct case(s) render them: {rendered:?}",
            variants.len(),
            rendered.len()
        ));
    }
    Ok(())
}
