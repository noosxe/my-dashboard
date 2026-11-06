//! `writes` / `assert-write` (docs/0010 D12): matching over the fixture
//! write record served by `app.writes`.

use serde_json::Value;

/// One `key=value` condition: `key` is a dotted path into the record's
/// `vars` (`input.mergeMethod`); `value` compares to a JSON string as
/// text, to anything else as its JSON rendering (`true`, `3`).
pub fn parse_pair(arg: &str) -> Result<(String, String), String> {
    match arg.split_once('=') {
        Some((k, v)) if !k.is_empty() => Ok((k.to_string(), v.to_string())),
        _ => Err(format!("expected key=value, got {arg:?}")),
    }
}

fn lookup<'a>(vars: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(vars, |v, part| match v {
        Value::Array(items) => part.parse::<usize>().ok().and_then(|i| items.get(i)),
        other => other.get(part),
    })
}

fn pair_holds(vars: &Value, key: &str, want: &str) -> bool {
    lookup(vars, key).is_some_and(|got| match got {
        Value::String(s) => s == want,
        other => format!("{other}") == want,
    })
}

/// The newest record with this `op` whose `vars` satisfy every pair.
pub fn find<'a>(writes: &'a [Value], op: &str, pairs: &[(String, String)]) -> Option<&'a Value> {
    writes
        .iter()
        .rev()
        .find(|w| w["op"] == op && pairs.iter().all(|(k, v)| pair_holds(&w["vars"], k, v)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn log() -> Vec<Value> {
        vec![
            json!({"seq":1,"kind":"graphql","op":"PrMerge",
                   "vars":{"input":{"pullRequestId":"PR_107","mergeMethod":"SQUASH"}}}),
            json!({"seq":2,"kind":"rest","op":"POST /x/approve","vars":null}),
        ]
    }

    #[test]
    fn matches_dotted_paths_and_op() {
        let w = log();
        let pairs = [
            parse_pair("input.mergeMethod=SQUASH").unwrap(),
            parse_pair("input.pullRequestId=PR_107").unwrap(),
        ];
        assert_eq!(find(&w, "PrMerge", &pairs).unwrap()["seq"], 1);
        assert!(find(&w, "PrClose", &[]).is_none());
        let wrong = [parse_pair("input.mergeMethod=MERGE").unwrap()];
        assert!(find(&w, "PrMerge", &wrong).is_none());
        assert!(find(&w, "POST /x/approve", &[]).is_some());
    }

    #[test]
    fn rejects_malformed_pairs() {
        assert!(parse_pair("novalue").is_err());
        assert!(parse_pair("=v").is_err());
    }
}
