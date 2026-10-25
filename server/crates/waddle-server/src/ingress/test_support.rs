use crate as waddle_server;

/// Model the room dispatcher's explicit canonical capture in synthetic plans.
pub(crate) fn capture_room_message(
    plan: &mut super::IngressPlan,
    message: &xmpp_parsers::message::Message,
) {
    plan.room_canonical_message = Some(Box::new(message.clone()));
}

/// One executable SQL block of the ingress-authority runbook, delimited by
/// `<!-- {marker}:begin -->` / `<!-- {marker}:end -->`.
///
/// The path is resolved from the runtime `CARGO_MANIFEST_DIR`, which the
/// nextest archive runner remaps to the relocated workspace; the nix lanes
/// copy the runbook there in `postUnpack`. Never skip on a missing file.
pub(crate) fn runbook_sql_block(marker: &str) -> String {
    let path = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("test runner sets CARGO_MANIFEST_DIR"),
    )
    .join("../../docs/operations/ingress-authority.md");
    let doc = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "read ingress-authority runbook at {} (is the flake.nix postUnpack copy intact?): {error}",
                path.display()
            )
        });
    let begin = format!("<!-- {marker}:begin -->");
    let end = format!("<!-- {marker}:end -->");
    let start = doc
        .find(&begin)
        .unwrap_or_else(|| panic!("runbook marker {begin} missing"))
        + begin.len();
    let stop = start
        + doc[start..]
            .find(&end)
            .unwrap_or_else(|| panic!("runbook marker {end} missing"));
    let block = &doc[start..stop];
    let fence = "```sql";
    let sql_start = block.find(fence).expect("marked block holds a sql fence") + fence.len();
    let sql_end = sql_start
        + block[sql_start..]
            .find("```")
            .expect("marked sql fence is closed");
    block[sql_start..sql_end].to_string()
}

/// Split a `$$`-free runbook block into its statements, comments removed.
pub(crate) fn runbook_statements(block: &str) -> Vec<String> {
    block
        .lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(str::to_string)
        .collect()
}

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/ingress_support.rs"
));
