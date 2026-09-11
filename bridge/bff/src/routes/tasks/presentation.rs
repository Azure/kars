use super::{NO_CHANGE_SENTINEL, RunBlockedDto, looks_scaffolded};

/// Classify a transport-`ok` run whose body is really a STOP condition (not a
/// deliverable). Today this recognises the daily token-budget block the router
/// enforces from the sandbox InferencePolicy — its message reads
/// "Daily token budget exceeded (23131/20000 tokens)". Returns `None` for a
/// genuine deliverable (or an already-`error` run, handled separately).
pub(crate) fn classify_blocked(status: Option<&str>, output: &str) -> Option<RunBlockedDto> {
    if status == Some("error") {
        return None;
    }
    let low = output.to_ascii_lowercase();
    let budget_hit = low.contains("token budget")
        && (low.contains("exceeded") || low.contains("429") || low.contains("budget at"));
    if budget_hit {
        let (spent, limit) = parse_budget_pair(output);
        return Some(RunBlockedDto {
            reason: "budget".into(),
            detail: "The run reached its daily token budget and stopped before finishing.".into(),
            spent,
            limit,
        });
    }
    None
}

/// Extract the `spent/limit` pair from a budget message like
/// "... (23131/20000 tokens)". Returns `(None, None)` when absent/unparseable.
fn parse_budget_pair(output: &str) -> (Option<i64>, Option<i64>) {
    // Find a "<digits>/<digits>" run (optionally followed by " tokens").
    let bytes = output.as_bytes();
    for (i, _) in output.match_indices('/') {
        // Walk left over digits.
        let mut l = i;
        while l > 0 && bytes[l - 1].is_ascii_digit() {
            l -= 1;
        }
        // Walk right over digits.
        let mut r = i + 1;
        while r < bytes.len() && bytes[r].is_ascii_digit() {
            r += 1;
        }
        if l < i && r > i + 1 {
            let spent = output[l..i].parse::<i64>().ok();
            let limit = output[i + 1..r].parse::<i64>().ok();
            if spent.is_some() && limit.is_some() {
                return (spent, limit);
            }
        }
    }
    (None, None)
}

/// Extract the human deliverable from the agent's run output. The native
/// OpenClaw agent returns a structured `--json` envelope
/// (`{ runId, status, summary, result: { payloads: [ { text } ] } }`); showing
/// that raw — escaped quotes, literal `\n`, JSON braces — is the single most
/// embarrassing thing in the UI. Pull out the actual prose (joining payload
/// texts), tolerating a few shapes; pass plain-text output through unchanged.
fn repair_replacement_question_marks(text: &str) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    let mut repaired = String::with_capacity(text.len());
    for (index, character) in characters.iter().copied().enumerate() {
        if character != '?' {
            repaired.push(character);
            continue;
        }
        let previous = index
            .checked_sub(1)
            .and_then(|at| characters.get(at))
            .copied();
        let next = characters.get(index + 1).copied();
        if previous.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric) {
            repaired.push('-');
        } else if previous.is_some_and(|value| value.is_ascii_digit())
            && next.is_some_and(char::is_whitespace)
        {
            repaired.push('.');
        } else {
            repaired.push('?');
        }
    }
    repaired
}

fn strip_sandbox_banner(text: &str) -> String {
    let lines = text.lines().collect::<Vec<_>>();
    let first_content = lines.iter().position(|line| !line.trim().is_empty());
    let Some(start) = first_content else {
        return String::new();
    };
    let prefix_end = (start + 16).min(lines.len());
    let prefix = &lines[start..prefix_end];
    let lower_prefix = prefix.join("\n").to_ascii_lowercase();
    if !lower_prefix.contains("kars sandbox")
        || !lower_prefix.contains("sandbox id:")
        || !lower_prefix.contains("security:")
        || !lower_prefix.contains("capabilities:")
    {
        return repair_replacement_question_marks(text.trim());
    }
    let Some(capabilities_offset) = prefix
        .iter()
        .position(|line| line.to_ascii_lowercase().contains("capabilities:"))
    else {
        return repair_replacement_question_marks(text.trim());
    };
    repair_replacement_question_marks(lines[start + capabilities_offset + 1..].join("\n").trim())
}

pub(crate) fn deliverable_text(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        // Native agent envelope: result.payloads[].text
        if let Some(payloads) = v
            .get("result")
            .and_then(|r| r.get("payloads"))
            .and_then(|p| p.as_array())
        {
            let joined = payloads
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n\n");
            if !joined.trim().is_empty() {
                return strip_sandbox_banner(&joined);
            }
        }
        // Other harness shapes.
        for path in [["reply", "text"], ["result", "text"]] {
            if let Some(t) = v
                .get(path[0])
                .and_then(|x| x.get(path[1]))
                .and_then(|t| t.as_str())
                && !t.trim().is_empty()
            {
                return strip_sandbox_banner(t);
            }
        }
        for key in ["text", "output", "summary"] {
            if let Some(t) = v.get(key).and_then(|t| t.as_str())
                && !t.trim().is_empty()
            {
                return strip_sandbox_banner(t);
            }
        }
    }
    // Tolerant fallback: a *truncated* native envelope (the commons caps stored
    // content, which can cut the JSON mid-string so `serde` can't parse it) still
    // begins like `{ "runId": ..., "result": { "payloads": [ { "text": "…` — pull
    // the first `"text"` string value out by hand and JSON-unescape it so old,
    // truncated entries render as prose instead of raw JSON.
    if trimmed.starts_with('{')
        && trimmed.contains("\"text\"")
        && let Some(extracted) = extract_first_json_string(trimmed, "text")
        && !extracted.trim().is_empty()
    {
        return strip_sandbox_banner(&extracted);
    }
    strip_sandbox_banner(raw)
}

/// A pull request the mission opened — a first-class deliverable type.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct PullRequestRef {
    /// `owner/repo`.
    pub repo: String,
    pub number: i64,
    /// The canonical GitHub URL.
    pub url: String,
}

/// Extract the pull requests a mission opened from its deliverable text. The
/// router authors PRs via the keyless git proxy and the agent reports the URL;
/// we surface each as a tracked deliverable. Deduplicated, in first-seen order.
pub(crate) fn extract_pull_requests(text: &str) -> Vec<PullRequestRef> {
    let mut out: Vec<PullRequestRef> = Vec::new();
    // Scan for `github.com/<owner>/<repo>/pull/<number>` occurrences without a
    // regex dep: split on the marker and parse each following segment.
    for seg in text.split("github.com/").skip(1) {
        // owner/repo/pull/NUMBER
        let mut it = seg.splitn(4, '/');
        let (Some(owner), Some(repo), Some(kind)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        if kind != "pull" && kind != "pulls" {
            continue;
        }
        let Some(rest) = it.next() else { continue };
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if owner.is_empty() || repo.is_empty() || num.is_empty() {
            continue;
        }
        let Ok(number) = num.parse::<i64>() else {
            continue;
        };
        let repo_full = format!("{owner}/{repo}");
        let url = format!("https://github.com/{repo_full}/pull/{number}");
        let pr = PullRequestRef {
            repo: repo_full,
            number,
            url,
        };
        if !out.contains(&pr) {
            out.push(pr);
        }
    }
    out
}

pub(super) fn deliverable_pull_requests(
    data: &std::collections::BTreeMap<String, String>,
) -> Vec<PullRequestRef> {
    let status = data.get("status").map(String::as_str);
    let output = data.get("output").map(String::as_str).unwrap_or("");
    if !is_real_deliverable(status, output) {
        return Vec::new();
    }
    extract_pull_requests(&deliverable_text(output))
}

pub(crate) fn is_failure_shaped_output(output: &str) -> bool {
    let text = deliverable_text(output);
    let lower = text
        .trim_start_matches(|character: char| {
            character.is_whitespace()
                || matches!(character, '*' | '_' | '#' | '>' | '`' | '-' | '?' | '🔒')
        })
        .to_ascii_lowercase();
    let head: String = lower.chars().take(800).collect();
    head.starts_with("unexpected tokens remaining in message header")
        || head.starts_with("assignment progress lease expired")
        || head.starts_with("native agent failed")
        || head.starts_with("error processing task")
        || (head.starts_with("kars sandbox - secure ai runtime") && head.contains("how can i help"))
        || head.starts_with("now await pr-watcher")
        || head.starts_with("awaiting handback from")
}

pub(crate) fn is_no_change_output(output: &str) -> bool {
    let text = deliverable_text(output);
    let head = text.trim_start();
    if head.starts_with(NO_CHANGE_SENTINEL) {
        return true;
    }
    let Some(sentinel_at) = head.find(NO_CHANGE_SENTINEL) else {
        return false;
    };
    let prefix = &head[..sentinel_at];
    sentinel_at <= 1_200
        && prefix.to_ascii_lowercase().contains("kars sandbox")
        && prefix.contains("Sandbox ID:")
        && prefix.contains("Security:")
        && prefix.contains("Capabilities:")
}

/// True when a run output is NOT a real, showable deliverable — either the run
/// errored, produced nothing, or reported "no material change". Used to keep
/// hung / zero-output / no-op runs out of the deliverable index and the "latest
/// deliverable" hero (audit f9/f13: a receipt/deliverable requires real work).
pub(crate) fn is_real_deliverable(status: Option<&str>, deliverable: &str) -> bool {
    if status == Some("error") {
        return false;
    }
    let t = deliverable.trim();
    if t.is_empty() {
        return false;
    }
    if is_no_change_output(deliverable) {
        return false;
    }
    if is_failure_shaped_output(deliverable) {
        return false;
    }
    // A capability/limit STOP (e.g. the daily token budget) came back transport-ok
    // but is not the mission's answer — never treat it as a deliverable.
    if classify_blocked(status, deliverable).is_some() {
        return false;
    }
    true
}

/// A clean 2–3 line preview of a deliverable for cards and list rows — never the
/// raw transcript. Strips the no-change sentinel, markdown table/heading noise,
/// and collapses whitespace, then caps the length (audit f3).
pub(crate) fn deliverable_excerpt(raw: &str) -> String {
    let text = deliverable_text(raw);
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        // Strip leading markdown wrapping (emphasis / heading / block-quote /
        // inline-code / bullet markers) FIRST, so a wrapped control sentinel
        // like `**[[NO_MATERIAL_CHANGE]]**` is unwrapped before we test for it.
        // Previously the sentinel check ran on the raw line and a bold-wrapped
        // sentinel slipped through into the excerpt.
        let cleaned = l
            .trim_start_matches(['*', '_', '#', '>', '`', '-', ' '])
            .trim();
        if cleaned.is_empty() {
            continue;
        }
        // Drop the no-change sentinel (now unwrapped) and markdown table
        // rows/rules.
        let cleaned = if let Some(reason) = cleaned.strip_prefix(NO_CHANGE_SENTINEL) {
            let reason = reason
                .trim_start_matches(|character: char| {
                    character.is_whitespace() || matches!(character, ':' | '-' | '—')
                })
                .trim();
            if reason.is_empty() {
                continue;
            }
            reason
        } else {
            cleaned
        };
        if cleaned.starts_with('|') {
            continue;
        }
        if cleaned.starts_with("===") {
            continue;
        }
        let lower = cleaned.to_ascii_lowercase();
        if [
            "kars sandbox - secure ai runtime",
            "foundry project:",
            "model:",
            "sandbox id:",
            "security summary",
            "security:",
            "capabilities:",
            "role plan",
            "role roster",
            "roles spawned:",
        ]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        {
            continue;
        }
        out.push(cleaned.to_string());
        if out.len() >= 3 {
            break;
        }
    }
    let joined = out.join(" ");
    let joined = joined.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > 240 {
        let mut s: String = joined.chars().take(240).collect();
        s.push('…');
        s
    } else {
        joined
    }
}

/// end of input. Returns `None` if the key/opening quote isn't present.
fn extract_first_json_string(s: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let after_key = &s[s.find(&needle)? + needle.len()..];
    let colon = after_key.find(':')?;
    let rest = &after_key[colon + 1..];
    let open = rest.find('"')?;
    let body = &rest[open + 1..];
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some(other) => out.push(other),
                None => break,
            },
            _ => out.push(c),
        }
    }
    Some(out)
}

/// Human-readable objective for display. A standing-run objective is wrapped
/// with internal scaffolding — `Standing-operation run for team 'X'. Charter:
/// <charter>. Your capabilities: … Operating contract: … --- BEGIN UNTRUSTED
/// REFERENCE DATA …` — none of which a person should see. Extract the charter /
/// intent and drop the capability manifest + injected prior-knowledge preamble.
/// Ordinary mission objectives (no wrapper) pass through unchanged.
pub(crate) fn clean_objective(raw: &str) -> String {
    // Everything from the first scaffolding marker onward is internal.
    const MARKERS: [&str; 5] = [
        "Your capabilities:",
        "Operating contract:",
        "--- BEGIN UNTRUSTED REFERENCE DATA",
        "\n\nMode note",
        "BEGIN UNTRUSTED REFERENCE DATA",
    ];
    let mut end = raw.len();
    for m in MARKERS {
        if let Some(i) = raw.find(m) {
            end = end.min(i);
        }
    }
    let head = raw[..end].trim();
    // Unwrap the standing-run charter prefix when present.
    if let Some(i) = head.find("Charter:") {
        let charter = head[i + "Charter:".len()..].trim();
        let charter = charter.trim_end_matches('.').trim();
        if !charter.is_empty() {
            return charter.to_string();
        }
    }
    // Defense in depth: strip any leaked 2026 loop scaffold so LOOP:/GOAL:/
    // CYCLE/[[…]] control-blobs never reach a title, card, or displayed
    // objective. A scaffold's GOAL line IS the human intent — extract it.
    strip_loop_scaffold(head)
}

/// Conversational lead-ins that mark a string as a prompt rather than a title
/// ("Can you please …", "I need you to …"). Stripped when deriving a title.
const TITLE_LEAD_INS: [&str; 16] = [
    "can you please ",
    "could you please ",
    "would you please ",
    "can you ",
    "could you ",
    "would you ",
    "please ",
    "i need you to ",
    "i want you to ",
    "i'd like you to ",
    "i would like you to ",
    "i need ",
    "i want ",
    "help me ",
    "let's ",
    "lets ",
];

/// Strip any leading conversational lead-in(s), case-insensitively.
fn strip_title_lead_in(s: &str) -> &str {
    let mut cur = s.trim_start();
    loop {
        let lower = cur.to_ascii_lowercase();
        let mut matched = false;
        for lead in TITLE_LEAD_INS {
            if lower.starts_with(lead) {
                cur = cur[lead.len()..].trim_start();
                matched = true;
                break;
            }
        }
        if !matched {
            return cur;
        }
    }
}

/// Shorten a bare URL token to a compact, human label — a GitHub-style
/// `owner/repo`, else the last path segment, else the host — so a title reads
/// "analyse Azure/kars dependabot PRs", not a 60-char URL.
fn shorten_url_token(tok: &str) -> String {
    let lower = tok.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return tok.to_string();
    }
    let rest = tok
        .trim_end_matches(['.', ',', ')', ']', '?', '!'])
        .split_once("://")
        .map(|x| x.1)
        .unwrap_or(tok);
    let mut parts = rest.split('/');
    let host = parts.next().unwrap_or("");
    let segs: Vec<&str> = parts.filter(|s| !s.is_empty()).collect();
    if host.contains("github.") && segs.len() >= 2 {
        format!("{}/{}", segs[0], segs[1])
    } else if let Some(last) = segs.last() {
        (*last).to_string()
    } else {
        host.to_string()
    }
}

/// True when `display` is a genuine human title, not a truncated prompt: it has
/// no conversational lead-in, carries no URL, isn't just a prefix of the
/// objective, and isn't paragraph-length.
fn is_genuine_title(display: &str, clean_objective: &str) -> bool {
    let lower = display.to_ascii_lowercase();
    if TITLE_LEAD_INS.iter().any(|l| lower.starts_with(l)) {
        return false;
    }
    if lower.contains("http://") || lower.contains("https://") {
        return false;
    }
    let d_trim = lower.trim_end_matches('…').trim();
    let obj_lower = clean_objective.to_ascii_lowercase();
    if d_trim.len() >= 24 && obj_lower.starts_with(d_trim) {
        return false;
    }
    display.chars().count() <= 72
}

/// Derive a compact, title-like phrase from a verbose objective: strip the
/// conversational lead-in, shorten URLs, take the first sentence/clause, drop a
/// trailing " - …" condition tail, cap at a word boundary, and capitalize.
pub(super) fn concise_title(text: &str) -> String {
    let no_lead = strip_title_lead_in(text.trim());
    let shortened: String = no_lead
        .split_whitespace()
        .map(shorten_url_token)
        .collect::<Vec<_>>()
        .join(" ");
    let first = shortened
        .split(['.', '\n', '?', '!'])
        .find(|s| !s.trim().is_empty())
        .unwrap_or(&shortened)
        .trim();
    // Prompts often append conditions after a dash ("… PRs - categorize the …").
    let first = first.split(" - ").next().unwrap_or(first).trim();
    let capped = if first.chars().count() > 56 {
        // Cut at the last word boundary within the cap.
        let head: String = first.chars().take(56).collect();
        let cut = head.rfind(' ').unwrap_or(head.len());
        format!("{}…", head[..cut].trim_end())
    } else {
        first.to_string()
    };
    let mut chars = capped.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// A clean, human display title for a task. Uses an explicit display name only
/// when it is a GENUINE title (not a conversational prompt truncated into the
/// display slot); otherwise derives a concise title from the cleaned objective.
/// Guarantees LOOP:/GOAL:/[[…]] and raw pasted prompts never reach a card, list
/// row, breadcrumb, or tab — it runs at the read/DTO boundary for every task.
pub(crate) fn clean_display_name(display: &Option<String>, objective: &str) -> Option<String> {
    let clean_obj = clean_objective(objective);
    if let Some(d) = display.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty())
        && !looks_scaffolded(d)
        && is_genuine_title(d, &clean_obj)
    {
        return Some(d.to_string());
    }
    // No genuine title — derive a concise one from the objective (or, when the
    // objective is empty, from the de-scaffolded display string).
    let source = if clean_obj.is_empty() {
        strip_loop_scaffold(display.as_deref().unwrap_or(""))
    } else {
        clean_obj.clone()
    };
    let title = concise_title(&source);
    if title.is_empty() { None } else { Some(title) }
}

fn strip_loop_scaffold(text: &str) -> String {
    if !looks_scaffolded(text) {
        return text.to_string();
    }
    // Prefer the GOAL line — that is the human's restated intent.
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("GOAL:") {
            let goal = rest
                .trim()
                .trim_start_matches("[[")
                .trim_end_matches("]]")
                .trim();
            if !goal.is_empty() {
                return goal.to_string();
            }
        }
    }
    // No GOAL line — drop the scaffold control lines and return the remainder.
    const CONTROL_PREFIXES: [&str; 6] = [
        "LOOP:",
        "CYCLE:",
        "SUCCESS:",
        "STOP:",
        "SUB-AGENT INHERITANCE",
        "[",
    ];
    let kept: Vec<&str> = text
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty() && !CONTROL_PREFIXES.iter().any(|p| t.starts_with(p))
        })
        .collect();
    kept.join(" ").trim().to_string()
}
