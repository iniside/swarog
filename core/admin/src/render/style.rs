use controlplane::Tone;
use super::*;

// ---------------------------------------------------------------------------
// Atoms
// ---------------------------------------------------------------------------

/// The avatar palette of the mockup's users (`UILayout/redesign/assets/data.js:4-17`).
const AVATAR_COLORS: [&str; 14] = [
    "#d9488a", "#4f5bd5", "#0f9488", "#c2630f", "#7c3aed", "#2563eb", "#be123c", "#65a30d", "#0891b2", "#9333ea",
    "#dc2626", "#059669", "#64748b", "#ca8a04",
];

fn fnv1a64(raw: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub(crate) fn avatar_color(seed: &str) -> &'static str {
    AVATAR_COLORS[(fnv1a64(seed) % AVATAR_COLORS.len() as u64) as usize]
}

/// The mockup's `initials` (`bundle.js:100`).
pub(crate) fn initials(name: &str) -> String {
    let kept: String = name.chars().filter(|c| c.is_ascii_alphabetic() || matches!(c, ' ' | '.' | '_')).collect();
    let out: String = kept
        .split([' ', '.', '_'])
        .filter(|p| !p.is_empty())
        .take(2)
        .filter_map(|p| p.chars().next())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if out.is_empty() {
        "?".to_string()
    } else {
        out
    }
}

/// The mockup's value-derived badge tone (`bundle.js:98`).
pub(super) fn status_tone(value: &str) -> &'static str {
    match value {
        "active" | "running" | "ok" | "sent" | "enabled" | "healthy" => "ok",
        "pending" => "info",
        "retrying" | "lagging" | "degraded" | "restarting" | "suspended" => "warn",
        "failed" | "fail" | "paused" | "down" | "crashed" | "banned" => "err",
        _ => "",
    }
}

pub(super) fn tone(t: Tone) -> &'static str {
    seen::record(&t);
    match t {
        Tone::Ok => "ok",
        Tone::Warn => "warn",
        Tone::Err => "err",
        Tone::Info => "info",
        Tone::Accent => "accent",
    }
}

pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// JavaScript's `+(n).toFixed(1)`: one decimal, a trailing `.0` dropped.
pub(super) fn fixed1(n: f64) -> String {
    let s = format!("{n:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}
