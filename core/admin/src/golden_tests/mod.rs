//! Golden renders of the portal templates: every primitive macro, the panel chrome in each
//! `PanelState`, the slot placeholders, the modals and their re-renders, the row menu and the
//! search palette. Each case is a view model built in code and rendered through the production
//! `render::panel` / `render::panel_form_result` / `render::modal` / `render::fragment` path;
//! action items come from `routes::action_item` / `routes::panel_tools`.
//!
//! These goldens prove the render layer only: what a given view model draws. They do not
//! prove route behaviour (which state a panel resolves to, which instance is minted, which
//! status a POST answers); that belongs in route tests.
//!
//! Each case is compared with `src/testdata/<name>.html`. Without `ADMIN_BLESS=1` a diff, a
//! missing file, or a `.html` file no case produces fails the run.
//! `ADMIN_BLESS=1 cargo test -p admin golden` rewrites the set, deletes the orphans and then
//! fails, so a leaked `ADMIN_BLESS` never turns the gate green; with `CI` set it refuses to
//! bless at all.
//!
//! Drift guards:
//! - every macro of every `templates/*.tmpl` is listed in [`MACRO_CASES`], checked against the
//!   templates both ways. Each entry carries a signature, a literal of that macro's source and
//!   of no other macro's, nor of any template's text outside its macros, but the ones the entry
//!   names, and every case it lists must render it. Every listed case must also run the macro:
//!   the cases render a second time through templates whose every macro opens with a marker
//!   comment of its own ([`marked_templates`]), the listed case's render must hold the macro's
//!   marker, and every marked render, markers removed, must equal the production render;
//! - [`primitive_case`] and [`state_case`] match every `Primitive` and `PanelState` variant with
//!   no wildcard arm, so a new variant does not compile until it names a case. The number of
//!   distinct rendered cases they name must equal the number of variants parsed from the enums'
//!   own source, so a new variant whose case is not rendered, or that reuses another variant's
//!   case, fails;
//! - every variant of every enum `controlplane::declared_enums` registers as drawn is converted
//!   into a view model by the render path while some case alone is built and rendered, as
//!   `render::seen` records it, and no undrawn or undeclared enum is
//!   ([`golden_renders_convert_every_drawn_enum_variant`]); every `pub enum` of controlplane is
//!   declared drawn or undrawn ([`every_controlplane_enum_is_declared_drawn_or_undrawn`]).
//!
//! Inputs are fixed: one csrf, one form id, fixed `age_s`, and no clock or id generator,
//! because the render layer reads neither.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use controlplane::{
    ActionDecl, ActionPoint, Align, AttentionItem, Axis, Banner, Cell, CellKind, Column, ConfirmSpec, ConfirmTone,
    ContributionDecl, DataTable, DeclaredEnum, Delta, Emphasis, EntityHeader, EntityRef, Field, FieldKind, FilterDef,
    FilterKind, FormSpec, KpiTile, Layout, Link, Manifest, Noun, PageInfo, Palette, PanelBody, Point, Primitive,
    PropertyList, QueueItem, QueueState, ReasonReq, Role, Row, SecretReveal, Series, ServiceItem, ServiceState, ServiceStatus,
    Severity, SortDir, Then, TimeRange, TimeSeriesChart, Timeline, TimelineEntry, Tone, Variants, WorkQueue,
};
use minijinja::Environment;

use crate::engine::actions::{self, Chrome};
use crate::engine::columns::{ColumnOutcome, ListColumn};
use crate::engine::labels::Labels;
use crate::engine::panels::{LastValue, PanelState};
use crate::engine::roles::Operator;
use crate::render::{
    self, ActionItem, ActionResultView, Crumb, DashboardView, EntityView, FlashView, FormFrame, FormInstance,
    LayoutRow, MenuView, ModalSpec, OperatorView, PageView, PanelInput, RevealView, SearchGroupView, SearchHitView,
    SearchView, ShellView, SlotView, TabView,
};
use crate::routes;

mod harness;
mod view_tests;
mod build;
mod cases;
mod variant_coverage;
mod macros;
mod declared;
mod selftests;

use self::harness::*;
use self::build::*;
use self::cases::*;
use self::variant_coverage::*;
use self::macros::*;
pub(crate) use self::view_tests::table_grid;

const CSRF: &str = "csrf-golden";
const MODULE: &str = "golden";
const CONTRIBUTION: &str = "panel";
const FORM_ID: &str = "form-golden-1";
/// A render's own newline in the golden form; see [`golden_form`].
const RAW_NEWLINE: char = '↵';
/// Unchanged lines shown around a difference.
const DIFF_CONTEXT: usize = 2;
/// Lines shown per side of a difference before the rest is counted.
const DIFF_MAX_LINES: usize = 12;

/// One rendered case with what [`golden_cases_cover_every_macro_variant_and_state`] checks.
struct Case {
    name: &'static str,
    golden: String,
    /// The render itself, which [`MACRO_CASES`] signatures are searched in.
    raw: String,
    /// [`primitive_case`] of a `render::panel` case's body.
    primitive: Option<&'static str>,
    /// [`state_case`] of a `render::panel` case drawn in panel chrome.
    state: Option<&'static str>,
    /// What the render path converted while this case alone was built and rendered.
    seen: render::seen::Record,
}

struct Rendered {
    name: &'static str,
    result: Result<String, minijinja::Error>,
    primitive: Option<&'static str>,
    state: Option<&'static str>,
    seen: render::seen::Record,
}

fn plain(name: &'static str, result: Result<String, minijinja::Error>) -> Rendered {
    Rendered { name, result, primitive: None, state: None, seen: render::seen::Record::new() }
}

/// `f`'s case with what the render path converted while `f` ran.
fn scoped(f: impl FnOnce() -> Rendered) -> Rendered {
    let (rendered, seen) = render::seen::collect(f);
    Rendered { seen, ..rendered }
}

fn goldens(cases: &[Case]) -> Vec<(String, String)> {
    cases.iter().map(|c| (c.name.to_string(), c.golden.clone())).collect()
}
